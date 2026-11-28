import assert from 'node:assert/strict';
import test from 'node:test';
import { withNativePage } from '../../../tools/native-page.ts';

async function motionBrowser(url: string) {
  const { cdpSession } = await import(new URL('../../../tools/e2e/cdp.mjs', import.meta.url).href) as {
    cdpSession(url: string): Promise<{ send(method: string, params: Record<string, unknown>): Promise<unknown>; close(): void }>;
  };
  const cdp = await cdpSession(url);
  try {
    await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
    return cdp;
  } catch (error) {
    cdp.close();
    throw error;
  }
}

test('ordered bars settle to the same pixels as a cold draw after worker playback', async () => {
  await withNativePage('ordered-bars-pixel-settle', '/ui/src/families/motionPixels.dom.fixture.ts', async page => {
    for (const form of [true, 'target-markers', 'ordered-segments']) {
      const { results, tinyEndpoints, histories, afterScroll, afterResize, afterTick, afterRefusal } = await page.evaluate<{
        results: { dpr: number; moving: number; paused: number }[];
        tinyEndpoints: { dpr: number; first: number; last: number }[];
        histories: { revoked: boolean; history: string; difference: number }[];
        afterScroll: number; afterResize: number; afterTick: number; afterRefusal: number;
      }>(`window.checkSettledPixels(${JSON.stringify(form)})`);
      for (const result of results) {
        const detail = `DPR ${result.dpr}, form ${form}`;
        assert.ok(result.moving > 0, `no movement at ${detail}`);
        assert.equal(result.paused, 0, `paused picture differs at ${detail}`);
      }
      if (form !== true) assert.equal(tinyEndpoints.length, 8);
      for (const { dpr, first, last } of tinyEndpoints) assert.deepEqual([first, last], [0, 0], `tiny target endpoints at DPR ${dpr}`);
      for (const sample of histories) assert.equal(sample.difference, 0, `${sample.history} with endpoint refusal ${sample.revoked}`);
      assert.equal(afterScroll, 0, 'layout must not copy other pixels into the owned slot');
      assert.equal(afterResize, 0, 'the worker redraws owned pixels as the resized geometry takes effect');
      assert.equal(afterTick, 0, 'the next tick at the same clock hour draws identical pixels');
      assert.equal(afterRefusal, 0, 'a proven unavailable hour matches its recorded cold picture');
    }
  });
});

test('a moving target clears the recorded readout until the plot widens at the next hour', async () => {
  await withNativePage('target-bars-readout-clearance', '/ui/src/families/motionPixels.dom.fixture.ts', async page => {
    type Edges = { barRight: number; readoutLeft: number };
    const { first, samples, last, firstDifference, lastDifference } = await page.evaluate<{
      first: Edges; samples: (Edges & { fraction: number; readoutDifference: number })[]; last: Edges; firstDifference: number; lastDifference: number;
    }>('window.checkReadoutClearance()');
    assert.ok(first.barRight > 0 && first.readoutLeft < 490 && last.readoutLeft < 490);
    assert.ok(last.barRight > first.readoutLeft, 'the next bar reaches into the previous readout column');
    assert.deepEqual([firstDifference, lastDifference], [0, 0], 'both recorded endpoints match cold draws');
    let previous = first.barRight;
    for (const { fraction, barRight, readoutLeft, readoutDifference } of samples) {
      assert.equal(readoutLeft, first.readoutLeft, `held readout at ${fraction}`);
      assert.ok(barRight > previous && barRight < readoutLeft, `bar ${barRight}, readout ${readoutLeft} at ${fraction}`);
      assert.equal(readoutDifference, 0, `recorded readout pixels at ${fraction}`);
      previous = barRight;
    }
  });
});

test('an ordered split confines a subpixel hatched tail to its painted edge', async () => {
  await withNativePage('ordered-segment-hatch-edge', '/ui/src/families/motionPixels.dom.fixture.ts', async page => {
    const samples = await page.evaluate<{ dpr: number; moving: boolean; inside: number; outside: number }[]>('window.checkSubpixelHatchTail()');
    for (const { dpr, moving, inside, outside } of samples) {
      const at = `${moving ? 'moving' : 'cold'} split at DPR ${dpr}`;
      assert.ok(inside > 0, `the hatch is visible inside the ${at}`);
      assert.equal(outside, 0, `background just beyond the ${at}`);
    }
  });
});

test('the shared host shows a proven refusal and reveals the held recorded bar after it expires', async () => {
  await withNativePage('ordered-bars-host-refusal', '/ui/src/families/motionHost.dom.fixture.tsx', async page => {
    const cdp = await motionBrowser(page.page);
    try {
      const result = await page.evaluate('window.checkHostedRefusal()');
      assert.deepEqual(result, { through: 100, painted: 100, rebound: true, resumed: true });
    } finally { cdp.close(); }
  });
});

test('a stationary pointer loses its tooltip on a painted row swap and recovers on installation', async () => {
  await withNativePage('ordered-bars-host-hover', '/ui/src/families/motionHost.dom.fixture.tsx', async page => {
    const cdp = await motionBrowser(page.page);
    try {
      const point = await page.evaluate<{ x: number; y: number }>('window.mountHostedHover()');
      await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...point });
      const result = await page.evaluate<{ before: string; behind: number; closed: boolean; replaced: boolean; after: string; installed: number }>('window.checkHostedHover()');
      assert.equal(result.before, 'GER');
      assert.equal(result.behind, 100);
      assert.equal(result.closed, true, 'the Germany tooltip closes before the newer rows install');
      assert.equal(result.installed, 101);
      assert.equal(result.replaced, true, 'the same pointer opens the new row tooltip with its recorded value');
      assert.equal(result.after, 'FRA');
    } finally { cdp.close(); }
  });
});
