import assert from 'node:assert/strict';
import test from 'node:test';
import { withNativePage, type NativePage } from '../../../../tools/native-page.ts';
import type { Exactness, Placement } from './pixelExact.dom.fixture.tsx';

type Cdp = { send(method: string, params?: Record<string, unknown>): Promise<unknown>; close(): void };

const RATIOS = [1, 1.25, 1.5, 1.75, 2];
const FAMILIES = ['bars', 'breakdown', 'card', 'flow', 'heat-lanes', 'heat-matrix', 'meters', 'table', 'time-chart', 'timeline'];

test('pausing a table restores the pixels of an exact fit at the same hour', async () => {
  await withNativePage('table-playback-widths', '/ui/src/families/host/pixelExact.dom.fixture.tsx', async page => {
    const pixels = await page.evaluate<{ playing: number; paused: number; correction: number }>('window.pixelExact.pausedTableFit()');
    assert.ok(pixels.playing > 0, 'the playing picture held the wider layout');
    assert.equal(pixels.paused, 0, 'the paused pixels match the fresh exact picture');
    assert.equal(pixels.correction, 0, 'a same-hour correction with different glyph widths also refits');
  }, [], false, ['--force-device-scale-factor=1.25']);
});

test('paused narrow fuel bars reserve the readout and its gutter at fractional DPR', async () => {
  await withNativePage('ordered-readout-gutter', '/ui/src/families/host/pixelExact.dom.fixture.tsx', async page => {
    const { dpr, plotLeft, plotRight, column, overlapPixels, nameInkInPlot } = await page.evaluate<{
      dpr: number; plotLeft: number; plotRight: number; column: number; overlapPixels: number; nameInkInPlot: number;
    }>('window.pixelExact.pausedFuelReadout(360)');
    assert.equal(dpr, 1.25);
    assert.equal(overlapPixels, 0, `the paused bar covers ${overlapPixels} pixels in the readout column`);
    assert.ok(column - plotRight >= 10);
    assert.ok(plotRight - plotLeft >= 40);
    assert.equal(nameInkInPlot, 0, 'the name fits when motion moves the plot to the left');
    const { readoutInkInGutter } = await page.evaluate<{ readoutInkInGutter: number }>(
      'window.pixelExact.pausedFuelReadout(360, undefined, [["100012345", "900000"], ["1000000000", "1000000000"]])',
    );
    assert.equal(readoutInkInGutter, 0, 'the five-decimal shortfall leaves room for the received amount inside the readout');
  }, [], false, ['--force-device-scale-factor=1.25']);
});

test('moving a paused canvas onto fractional device pixels keeps its installed answer while drawing is held', async () => {
  await withNativePage('pixel-exact-paused', '/ui/src/families/host/pixelExact.dom.fixture.tsx', async page => {
    assert.ok((await page.evaluate<{ settled: boolean }>('window.pixelExact.mount()')).settled);
    await page.evaluate('window.pixelExact.placeEmptyBars()');
    assert.ok((await page.evaluate<{ settled: boolean }>('window.pixelExact.settle()')).settled);
    const before = await page.evaluate<{ id: number; busy: string | null }>('window.pixelExact.barsPicture()');
    assert.ok(before.id);
    assert.equal(before.busy, null);
    try {
      await page.evaluate('window.pixelExact.hold(); window.pixelExact.moveEmptyBars(); window.pixelExact.frames(6)');
      assert.deepEqual(await page.evaluate('window.pixelExact.barsPicture()'), before);
      const bars = (await page.evaluate<Placement[]>('window.pixelExact.placements()')).find(row => row.id === 'bars');
      assert.ok(bars);
      assert.ok(bars.origin.every(value => Math.abs(value - Math.round(value)) <= 0.01));
      assert.deepEqual(bars.backing, bars.box);
    } finally {
      await page.evaluate('window.pixelExact.release()');
    }
    const cdp = await cdpFor(page);
    try {
      await cdp.send('Emulation.setDeviceMetricsOverride', { width: 0, height: 0, deviceScaleFactor: 1, mobile: false });
      const edge = await page.evaluate<{ offset: number; inside: boolean }>('window.pixelExact.barsLeadingEdgeAtSmallScale()');
      assert.ok(edge.offset < -1, `the canvas moves ${edge.offset} local CSS pixels above the body`);
      assert.ok(edge.inside, 'the leading paint edge stays inside the extent');
    } finally { cdp.close(); }
  }, [], false, ['--force-device-scale-factor=1.25', '--window-size=1440,1000']);
});

async function cdpFor(page: NativePage): Promise<Cdp> {
  const { cdpSession } = await import(new URL('../../../../tools/e2e/cdp.mjs', import.meta.url).href) as { cdpSession(url: string): Promise<Cdp> };
  return cdpSession(page.page);
}

async function screenshot(cdp: Cdp): Promise<string> {
  return (await cdp.send('Page.captureScreenshot', { format: 'png' }) as { data: string }).data;
}

async function measure(page: NativePage, cdp: Cdp): Promise<Exactness[]> {
  const shown = await screenshot(cdp);
  await page.evaluate('window.pixelExact.hideCanvases(true)');
  const under = await screenshot(cdp);
  await page.evaluate('window.pixelExact.hideCanvases(false)');
  return page.evaluate<Exactness[]>(`window.pixelExact.exactness(${JSON.stringify(shown)}, ${JSON.stringify(under)})`);
}

for (const ratio of RATIOS) test(`at ${ratio}, text anchors and canvas origins stay on device pixels below odd chrome, scrolled and mid-glide`, async t => {
  await withNativePage('pixel-exact-text', '/ui/src/families/host/pixelExact.dom.fixture.tsx', page => page.stage('text exactness', async () => {
    const cdp = await cdpFor(page);
    try {
      await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
      const mounted = await page.evaluate<{ settled: boolean }>('window.pixelExact.mount(true)');
      assert.ok(mounted.settled);
      await page.evaluate('window.pixelExact.oddHeader(); window.pixelExact.scrollTable(13.3)');
      await page.evaluate('window.pixelExact.settle()');
      const still = (await measure(page, cdp)).filter(row => ['bars', 'table', 'time-chart'].includes(row.id));
      for (let step = 1; step <= 11; step++) {
        await page.evaluate(`window.pixelExact.nudgeDashboard(${step * 0.3}); window.pixelExact.frames(3)`);
        const placed = await page.evaluate<Placement[]>('window.pixelExact.placements()');
        for (const row of placed) assert.ok(row.offset.every(v => Math.abs(v) <= 0.51), `${row.id}: small move ${step} stays within half a device pixel of its body (${row.offset})`);
      }
      await page.evaluate('window.pixelExact.nudgeDashboard(0); window.pixelExact.frames(3)');
      await page.evaluate('window.pixelExact.move(); window.pixelExact.resize("table", 4, 9, 3.4); window.pixelExact.tallTable(); window.pixelExact.scrollDashboard(37.3)');
      await page.evaluate('window.pixelExact.settle()');
      const moved = (await measure(page, cdp)).filter(row => ['bars', 'table', 'time-chart'].includes(row.id));
      await page.evaluate('window.pixelExact.glide()');
      const motion = await page.evaluate<Record<string, { result: unknown; now: number; changedAt: number; before: number; hour: number }>>('window.pixelExact.motionFrames()');
      assert.equal(motion.bars!.result, true, 'bars are gliding');
      assert.deepEqual(motion.table!.result, { drew: true, next: true }, 'table rows are gliding');
      for (const id of ['bars', 'table']) {
        assert.ok(motion[id]!.before < motion[id]!.hour, `${id}: a preceding recorded hour supplies the glide`);
        assert.equal(motion[id]!.now - motion[id]!.changedAt, 67, `${id}: fixed time inside the glide`);
      }
      assert.equal(await page.evaluate('window.pixelExact.playbackLayoutReads()'), 0, 'playing frames read no layout');
      const moving = (await measure(page, cdp)).filter(row => ['bars', 'table', 'time-chart'].includes(row.id));
      const rows = [...still.map(row => ({ frame: 'still', ...row })), ...moved.map(row => ({ frame: 'moved/resized', ...row })), ...moving.map(row => ({ frame: 'glide', ...row }))];
      t.diagnostic(JSON.stringify(rows.map(({ frame, id, origin, textDraws, fractionalText, textError, differing, compared, samples }) => ({ frame, id, origin, textDraws, fractionalText, textError, differing, compared, samples }))));
      for (const row of rows) {
        assert.ok(row.textDraws > 0, `${row.frame} ${row.id}: text was measured`);
        assert.ok(row.origin.every(v => Math.abs(v - Math.round(v)) <= 0.01), `${row.frame} ${row.id}: fractional origin ${row.origin}`);
        assert.equal(row.fractionalText, 0, `${row.frame} ${row.id}: ${row.fractionalText} text anchors off device pixels by up to ${row.textError}`);
        assert.equal(row.differing, 0, `${row.frame} ${row.id}: ${row.differing} resampled pixels`);
      }
    } finally { cdp.close(); }
  }, 240_000), [], false, [`--force-device-scale-factor=${ratio}`, '--window-size=1440,1000']);
});

async function exactOnScreen(page: NativePage, cdp: Cdp, label: string, settle = true): Promise<Exactness[]> {
  if (settle) {
    const settled = await page.evaluate<{ settled: boolean; pending: string[] }>('window.pixelExact.settle()');
    assert.ok(settled.settled, `${label}: every widget installed its picture (${settled.pending.join(', ')} pending)`);
  }
  const shown = await screenshot(cdp);
  await page.evaluate('window.pixelExact.hideCanvases(true)');
  const under = await screenshot(cdp);
  await page.evaluate('window.pixelExact.hideCanvases(false)');
  const rows = await page.evaluate<Exactness[]>(`window.pixelExact.exactness(${JSON.stringify(shown)}, ${JSON.stringify(under)})`);
  assert.deepEqual(rows.map(row => row.id).sort(), FAMILIES, `${label}: one canvas per family`);
  for (const row of rows) {
    assert.ok(row.compared > 0, `${label}, ${row.id}: some of the canvas is on screen to compare`);
    assert.deepEqual(row.backing, row.box, `${label}, ${row.id}: the backing ${row.backing.join('×')} is the canvas's device-pixel box ${row.box.join('×')}`);
    assert.deepEqual(row.backing, row.css.map(css => Math.round(css * row.dpr)), `${label}, ${row.id}: the backing is the CSS size × ${row.dpr}`);
    assert.ok(row.css.every((css, axis) => Math.abs(css * row.dpr - row.backing[axis]!) < 1e-5), `${label}, ${row.id}: the CSS size ${row.css.join('×')} is whole device pixels at ${row.dpr}`);
    assert.equal(row.differing, 0, `${label}, ${row.id}: ${row.differing} of ${row.compared} pixels on screen differ from the painter's bitmap by more than 1 level (worst ${row.worst}); canvas at device ${row.origin.map(v => v.toFixed(3)).join(', ')}; ${row.samples.join('; ')}`);
  }
  return rows;
}

for (const ratio of RATIOS) {
  test(`at device pixel ratio ${ratio}, every family's canvas shows the painter's pixels exactly: placed by the grid at fractional offsets, with the dashboard and a table scrolled by fractional amounts, after a resize and under a zoom`, async t => {
    await withNativePage(`pixel-exact-${String(ratio).replace('.', '-')}`, '/ui/src/families/host/pixelExact.dom.fixture.tsx', page => page.stage('pixel exact', async () => {
      const cdp = await cdpFor(page);
      try {
        await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
        const mounted = await page.evaluate<{ settled: boolean; pending: string[]; dpr: number }>('window.pixelExact.mount()');
        assert.equal(mounted.dpr, ratio, 'the browser runs at the device pixel ratio under test');
        assert.ok(mounted.settled, `every widget drew (${mounted.pending.join(', ')} pending)`);

        const placed = await exactOnScreen(page, cdp, 'placed');
        t.diagnostic(`placed: ${placed.map(row => `${row.id} ${row.backing.join('×')} at ${row.origin.map(v => v.toFixed(2)).join(',')}`).join('; ')}`);

        const dashboardTop = await page.evaluate<number>('window.pixelExact.scrollDashboard(37.3)');
        assert.ok(dashboardTop > 0, 'the dashboard scrolled');
        await exactOnScreen(page, cdp, `dashboard scrolled to ${dashboardTop}`);

        const tableTop = await page.evaluate<number>('window.pixelExact.scrollTable(13.3)');
        assert.ok(tableTop > 0, 'the table scrolled');
        await exactOnScreen(page, cdp, `table scrolled to ${tableTop}`);

        await page.evaluate('window.pixelExact.hold()');
        await page.evaluate('window.pixelExact.resize("breakdown", 5, 9, -7.4)');
        await page.evaluate('window.pixelExact.frames(4)');
        const resizing = await exactOnScreen(page, cdp, 'resizing, before the new pictures arrive', false);
        assert.deepEqual(resizing.find(row => row.id === 'breakdown')!.backing, placed.find(row => row.id === 'breakdown')!.backing, 'while resizing, the old picture is still the one shown');
        await page.evaluate('window.pixelExact.release()');
        const resized = await exactOnScreen(page, cdp, 'resized');
        const before = placed.find(row => row.id === 'breakdown')!;
        assert.notDeepEqual(resized.find(row => row.id === 'breakdown')!.backing, before.backing, 'the resized widget has a new backing');

        await page.evaluate('window.pixelExact.hold()');
        await page.evaluate('window.pixelExact.zoom({ card: 1.1, bars: 0.9, table: 1.25 })');
        await page.evaluate('window.pixelExact.frames(4)');
        const rezooming = await exactOnScreen(page, cdp, 'zoom changed, before the new pictures arrive', false);
        assert.deepEqual(rezooming.find(row => row.id === 'card')!.backing, resized.find(row => row.id === 'card')!.backing, 'while the zoom changes, the old picture is still the one shown');
        await page.evaluate('window.pixelExact.release()');
        const zoomed = await exactOnScreen(page, cdp, 'zoomed');
        assert.ok(Math.abs(zoomed.find(row => row.id === 'card')!.dpr - ratio * 1.1) < 1e-6, 'a zoomed widget paints at its own device scale');

        if (ratio === 1) {
          await cdp.send('Emulation.setDeviceMetricsOverride', { width: 0, height: 0, deviceScaleFactor: 1.5, mobile: false });
          await page.evaluate('window.pixelExact.settle()');
          const moved = await page.evaluate<Placement[]>('window.pixelExact.placements()');
          for (const row of moved) {
            assert.ok(Math.abs(row.dpr / (zoomed.find(item => item.id === row.id)!.dpr) - 1.5) < 1e-6, `${row.id}: a new device pixel ratio reaches the painter`);
            assert.deepEqual(row.backing, row.css.map(css => Math.round(css * row.dpr)), `${row.id}: after the ratio changes, the backing is the CSS size × ${row.dpr}`);
          }
        }
      } finally {
        cdp.close();
      }
    }, 240_000), [], false, [`--force-device-scale-factor=${ratio}`, '--window-size=1440,1000']);
  });
}
