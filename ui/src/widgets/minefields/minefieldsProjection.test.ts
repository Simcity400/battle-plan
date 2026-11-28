import assert from 'node:assert/strict';
import test from 'node:test';
import { liveKey } from '../../charts/live.ts';
import { valuesOf } from '../../test-support/liveValues.ts';
import { assertBarsModelValid, deriveBarsAudit, type BarsDataRow } from '../../families/bars/model.ts';
import { createBoundedReadView, displayedHour, FutureDataError } from '../../families/boundedView.ts';
import type { IdentityProvider } from '../../families/plan.ts';
import type { MinefieldsReply } from '../../generated/dashboard';
import { minefieldsProjection } from './minefieldsProjection.ts';

function fakeIdentity(names: Record<string, { name: string; color: string }>): IdentityProvider {
  return {
    resolve(kind, key, atHour) {
      const hit = names[kind === 'country' ? key : `${kind}:${key}`];
      return {
        kind,
        key,
        atHour,
        holder: { status: 'ready', value: key },
        tag: { status: 'ready', value: key },
        name: hit ? { status: 'ready', value: hit.name } : { status: 'absent', reason: 'Name not recorded' },
        colour: hit ? { status: 'ready', value: hit.color } : { status: 'absent', reason: 'Colour not recorded' },
        validInterval: [0, 1000],
        nextChangeAt: Infinity,
      };
    },
  };
}

const REPLY: MinefieldsReply = {
  regions: [
    { region: 7, owners: [], unknownOwners: 0, pinned: true, missing: "Some countries' minefields were not read at this hour" },
    { region: 3, owners: [{ tag: 'ENG' }, { tag: 'GER' }], unknownOwners: 0, pinned: false, missing: null },
    { region: 5, owners: [], unknownOwners: 2, pinned: false, missing: null },
  ],
  unknown: [],
  empty: null,
  activeUnlisted: { ids: [], missing: null },
  scale: 100000,
  warnings: [],
};

test("minefields projection projects owner shares from Rust live totals, the user's regions first, and labels mines clearly", () => {
  const identity = fakeIdentity({
    ENG: { name: 'United Kingdom', color: '#e11d48' },
    GER: { name: 'German Reich', color: '#6b7280' },
    'region:3': { name: 'North Sea', color: '' },
    'region:5': { name: 'Baltic Sea', color: '' },
    'region:7': { name: 'Skagerrak', color: '' },
  });
  const live = valuesOf(new Map([
    [liveKey('3', 'ENG'), 900000],
    [liveKey('3', 'GER'), 500000],
    [liveKey('3', 'total'), 1400000],
  ]), 'Not recorded at this hour');

  const view = createBoundedReadView({
    hour: displayedHour(350),
    panels: new Map([['main', REPLY]]),
    live: new Map([['main', live]]),
    identity,
  });

  const model = minefieldsProjection.project(view, { surface: '#1e1e1e', order: 'mines' });
  assertBarsModelValid(model, 350);
  assert.equal(model.form, 'share');

  assert.deepEqual(model.rows.map(r => [r.kind, r.key]), [
    ['section', '__section:picked'], ['data', '7'], ['section', '__section:auto'], ['data', '3'], ['data', '5'], ['add-region', '__add-region'],
  ]);
  const rows = model.rows.filter((r): r is BarsDataRow => r.kind === 'data').slice(1);
  const pick = model.rows[1] as BarsDataRow;
  assert.deepEqual([pick.label, pick.region, pick.group, pick.value?.value.kind], [{ kind: 'known', value: 'Skagerrak' }, 7, 'picked', 'notRecorded'], 'an unread pick is not shown as no mines');
  assert.deepEqual(rows.map(r => [r.key, r.label, r.region]), [['3', { kind: 'known', value: 'North Sea' }, 3], ['5', { kind: 'known', value: 'Baltic Sea' }, 5]]);
  assert.equal(rows[0]!.readout?.primary, '14 mines');
  assert.deepEqual(rows[0]!.segments?.map(s => [s.key, s.label, Math.round(s.share * 100)]), [
    ['ENG', 'United Kingdom', 64],
    ['GER', 'German Reich', 36],
  ]);

  assert.equal(rows[1]!.value?.value.kind, 'notRecorded');
  assert.equal(rows[1]!.readout?.primary, 'Not recorded');

  const audit = deriveBarsAudit(model);
  assert.ok(audit.some(entry => entry.row === '3' && entry.key === '3:segment:ENG'));
  assert.ok(audit.some(entry => entry.row === '5' && entry.key === '5:value' && entry.value?.startsWith('["notRecorded"')));
});

test('minefields projection marks truncated total as notRecorded rather than zero and rejects future hours', () => {
  const live = valuesOf(new Map([[liveKey('3', 'ENG'), 400000]]), 'Naval regional mines truncated');
  const view = createBoundedReadView({
    hour: displayedHour(100),
    panels: new Map([['main', REPLY]]),
    live: new Map([['main', live]]),
  });

  const model = minefieldsProjection.project(view, { surface: '#1e1e1e', order: 'mines' });
  assertBarsModelValid(model, 100);
  const first = model.rows.find(row => row.key === '3') as BarsDataRow;
  assert.deepEqual(first.label, { kind: 'pending', reason: 'Name is loading' });
  assert.equal(first.value?.value.kind, 'notRecorded');
  assert.equal(first.segments?.length, 0);
  assert.equal(first.readout?.primary, 'Not recorded');
  const audit = deriveBarsAudit(model);
  assert.ok(audit.some(entry => entry.key === '3:label' && entry.value === '["pending","Name is loading"]'));
  assert.throws(() => view.who('country', 'ENG', displayedHour(101)), FutureDataError);
});

test('minefields retains a known share while marking an incomplete owner breakdown', () => {
  const base = REPLY.regions[1]!;
  const hour = displayedHour(350);
  for (const unreadOwner of [false, true]) {
    const live = valuesOf(new Map([
      [liveKey('3', 'ENG'), 900000],
      [liveKey('3', 'total'), 1400000],
    ]), 'Not recorded at this hour');
    const regions = [{
      ...base,
      owners: unreadOwner ? base.owners : base.owners.slice(0, 1),
      unknownOwners: unreadOwner ? 0 : 1,
    }];
    const view = createBoundedReadView({ hour, panels: new Map([['main', { ...REPLY, regions }]]), live: new Map([['main', live]]) });
    const row = minefieldsProjection.project(view, { surface: '#1e1e1e', order: 'mines' }).rows.find(item => item.kind === 'data')!;
    assert.deepEqual(row.segments?.map(segment => segment.key), ['ENG']);
    assert.equal(row.value, undefined);
    assert.equal(row.segmentsIncomplete, true);
  }
});

test('an owner without a recorded colour keeps one colour whichever place its amount gives it', () => {
  const identity = fakeIdentity({ GER: { name: 'German Reich', color: '#6b7280' } });
  const colourOf = (owners: readonly string[], amounts: readonly number[]) => {
    const live = valuesOf(new Map([
      ...owners.map((tag, at) => [liveKey('3', tag), amounts[at]!] as const),
      [liveKey('3', 'total'), amounts.reduce((sum, amount) => sum + amount, 0)],
    ]), 'Not recorded at this hour');
    const reply: MinefieldsReply = { ...REPLY, regions: [{ region: 3, owners: owners.map(tag => ({ tag })), unknownOwners: 0, pinned: false, missing: null }] };
    const view = createBoundedReadView({ hour: displayedHour(350), panels: new Map([['main', reply]]), live: new Map([['main', live]]), identity });
    const row = minefieldsProjection.project(view, { surface: '#1e1e1e', order: 'mines' }).rows.find(item => item.key === '3') as BarsDataRow;
    return row.segments!.find(segment => segment.key === 'ENG')!.color;
  };
  assert.equal(colourOf(['ENG', 'GER'], [900000, 500000]), colourOf(['GER', 'ENG'], [900000, 500000]));
});

test('hiding the only mined region is said as such, never as no minefields', () => {
  const body = (empty: MinefieldsReply['empty']) => {
    const view = createBoundedReadView({ hour: displayedHour(350), panels: new Map([['main', { ...REPLY, regions: [], empty }]]) });
    const model = minefieldsProjection.project(view, { surface: '#1e1e1e', order: 'mines' });
    return [model.empty, model.emptyKind];
  };
  assert.deepEqual(body({ kind: 'filtered', hidden: 1, outside: 0 }), ['The only mined region at this date is removed from this chart.', 'empty']);
  assert.deepEqual(body({ kind: 'unread', reason: "Some countries' minefields were not read at this hour" }), ["Some countries' minefields were not read at this hour.", 'unavailable']);
  assert.deepEqual(body({ kind: 'none' }), ['No minefields at this date.', 'empty']);
});

test('sorted by name, the automatic rows are not headed "Most active"', () => {
  const view = createBoundedReadView({ hour: displayedHour(350), panels: new Map([['main', REPLY]]) });
  const label = (order: 'mines' | 'name') => minefieldsProjection.project(view, { surface: '#1e1e1e', order }).rows.find(row => row.key === '__section:auto')?.label;
  assert.deepEqual([label('mines'), label('name')], ['Most active', 'Mined regions']);
});
