import assert from 'node:assert/strict';
import test from 'node:test';
import { countryKey } from '../../dashboard/countryKey.ts';
import { assertBarsModelValid, deriveBarsAudit, deriveBarsMirror, type BarsDataRow } from '../../families/bars/model.ts';
import { createBoundedReadView, displayedHour, FutureDataError } from '../../families/boundedView.ts';
import type { IdentityProvider } from '../../families/plan.ts';
import { knownValue } from '../../families/value.ts';
import type { SeriesRankReply } from '../../generated/dashboard';
import { gameScoreRaceDeclaration, gameScoreRaceProjection, type GameScoreRaceOptions } from './gameScoreRaceProjection.ts';

function fakeIdentity(names: Record<string, string>): IdentityProvider {
  return {
    resolve(kind, key, atHour) {
      const hit = kind === 'country' ? names[key] : undefined;
      return {
        kind,
        key,
        atHour,
        holder: { status: 'ready', value: key },
        tag: { status: 'ready', value: key },
        name: hit ? { status: 'ready', value: hit } : { status: 'absent', reason: 'Name not recorded' },
        colour: { status: 'ready', value: '#6b7280' },
        flagChoice: { status: 'absent', reason: 'Flag not recorded' },
        validInterval: [0, 1000],
        nextChangeAt: Infinity,
      };
    },
  };
}

const MEASURES = ['total', 'army', 'navy', 'air'] as const;
const reading = (country: string, raw: number | null) => ({
  country, hour: 500, drop: null, missing: raw === null ? 'Scores were not read at this hour' : null,
  run: raw === null ? null : { firstHour: 490, lastHour: 510, valueRaw: String(raw * 100_000), slot: 0, countryId: 0, startsSegment: false, segmentFirstHour: 0, firstExact: true, lastExact: true },
});
const reply = (rows: readonly (readonly [string, number, ...(number | null)[]])[], refused: string | null = null): SeriesRankReply => ({
  branchId: '1', hour: 500, from: 500, until: 500, refused,
  measures: MEASURES.map(name => ({ name, decimals: 5 })),
  rows: rows.map(([country, slot, ...values]) => ({ country, slot, readings: values.map(value => reading(country, value)) })),
});
const view = (panel: SeriesRankReply) => createBoundedReadView({
  hour: displayedHour(500),
  panels: new Map([['main', panel]]),
  identity: fakeIdentity({ [countryKey(1)]: 'United States', [countryKey(2)]: 'German Reich', [countryKey(3)]: 'United Kingdom' }),
});
const OPTIONS: GameScoreRaceOptions = { surface: '#1e1e1e', top: 10, all: false, group: false, by: 'total', split: true };

test('game-score race keeps the product order, keys rows by recorded slot, and splits only a proven sum of parts', () => {
  const shown = view(reply([['USA', 1, 4000, 1500, 1500, 1000], ['GER', 2, 3500, 2000, 500, null], ['ENG', 3, 2500, 500, 2250.125, -250.125]]));
  const model = gameScoreRaceProjection.project(shown, OPTIONS);
  assertBarsModelValid(model, 500);
  const rows = model.rows.filter((row): row is BarsDataRow => row.kind === 'data');
  assert.deepEqual(rows.map(row => [row.key, row.label, row.numeric]), [
    [countryKey(1), knownValue('United States'), 4000],
    [countryKey(2), knownValue('German Reich'), 3500],
    [countryKey(3), knownValue('United Kingdom'), 2500],
  ]);
  assert.deepEqual(rows[0]!.segments?.map(segment => [segment.key, Math.round(segment.share * 100)]), [['army', 38], ['navy', 38], ['air', 25]]);
  assert.equal(rows[1]!.segments, undefined, 'a missing part leaves the bar whole: the known parts are never stretched to fill it');
  assert.equal(rows[1]!.segmentsIncomplete, true);
  assert.deepEqual(rows[1]!.tooltip?.items?.find(item => item.label === 'Air'), { label: 'Air', swatch: rows[0]!.segments![2]!.color, missingReason: 'Scores were not read at this hour' });
  assert.deepEqual(rows[2]!.segments?.map(segment => segment.key), ['army', 'navy']);
  assert.equal(rows[2]!.tooltip?.items?.find(item => item.label === 'Air')?.value, '\u2212250.125');
  assert.match(deriveBarsMirror(model).rows[2]?.cells.find(cell => cell.column === 'segments')?.spoken ?? '', /Air \u2212250\.125/);
  assert.equal(deriveBarsAudit(model).find(cell => cell.key === `${countryKey(3)}:part:air`)?.value, '["known",["int","-25012500"]]');
  assert.throws(() => shown.who('country', 'GER', displayedHour(501)), FutureDataError);
});

test('a refused top says why, and chosen countries keep the product order with the unrecorded last', () => {
  const refused = gameScoreRaceProjection.project(view(reply([], 'GER’s total was not recorded at this hour')), OPTIONS);
  assert.deepEqual([refused.emptyKind, refused.empty], ['unavailable', 'GER’s total was not recorded at this hour']);
  const chosen = gameScoreRaceProjection.project(view(reply([['ENG', 3, 1200, null, null, null], ['GER', 2, null, null, null, null]], 'GER’s total was not recorded at this hour')), { ...OPTIONS, top: null, split: false });
  const rows = chosen.rows.filter((row): row is BarsDataRow => row.kind === 'data');
  assert.deepEqual(rows.map(row => [row.key, row.value?.value.kind]), [[countryKey(3), 'known'], [countryKey(2), 'notRecorded']]);
  const split = gameScoreRaceProjection.project(view(reply([['ENG', 3, null, 300, null, -50]])), { ...OPTIONS, top: null });
  assert.equal((split.rows[0] as BarsDataRow).segments, undefined);
  assert.equal(deriveBarsAudit(split).find(cell => cell.key === `${countryKey(3)}:part:air`)?.value, '["known",["int","-5000000"]]');
  assert.match(deriveBarsMirror(split).rows[0]?.cells.find(cell => cell.column === 'segments')?.spoken ?? '', /Navy Not recorded.*Air \u221250/i);
});

test('the race asks the one rank reply, with the split parts only when splitting the total, and sends a group whole', () => {
  const ctx = (options: Record<string, unknown>, countries: unknown) => ({
    track: { source: { id: 's', revision: 1 }, axisOffset: 0 },
    branch: { id: '1' },
    options,
    countries: { top: null, all: false, every: [], tags: ['GER'], membership: null, holders: null, unobserved: [] },
    widget: { countries },
  }) as unknown as Parameters<typeof gameScoreRaceDeclaration.sources>[0];
  const asked = (options: Record<string, unknown>, countries: unknown) => {
    const read = gameScoreRaceDeclaration.sources(ctx(options, countries), { following: true, staticSpan: null }).entries[0]!.read;
    assert.ok(read.shape === 'point' && read.product === 'series-rank');
    return read.shape === 'point' ? read.followQuery as Record<string, unknown> : {};
  };
  const tags = { kind: 'tags', tags: ['GER'] };
  assert.deepEqual(asked({ topMeasure: 'total' }, tags).measures, ['army', 'navy', 'air']);
  assert.equal(asked({ topMeasure: 'army' }, tags).measures, undefined);
  const faction = asked({ topMeasure: 'army' }, { kind: 'group', group: { type: 'faction', id: '42' }, membership: 'hour', aggregate: false });
  assert.deepEqual([faction.group, faction.countries], [{ type: 'faction', id: '42', membership: 'hour' }, undefined]);
});
