import { readableColour } from '../charts/palette.ts';
import { datedPanel, datedRecord } from '../dashboard/follow/messages.fixture.ts';
import type { Refusal } from '../dashboard/follow/page.ts';
import { drawSplitBar } from '../draw/kit.ts';
import { Lane, type Presented } from '../draw/lane.ts';
import { parseColor } from '../shell/appearance/color.ts';
import { DEFAULT_SEEDS, deriveColors } from '../shell/appearance/themes.ts';
import { defaultArtFrame, readyArt } from './art/types.ts';
import { barsLane, type FamilyBarsBinding } from './bars/laneView.ts';
import type { BarsModel } from './bars/model.ts';
import type { BarsPlaybackForm, FamilyTheme } from './contract.ts';
import { createLaneView } from './laneRuntime.ts';
import { registerFamilyProjection } from './registry.ts';
import { knownValue } from './value.ts';

const colours = deriveColors(DEFAULT_SEEDS.dark);
const theme: FamilyTheme = {
  surface: colours.surface, raised: colours.raised, dark: true, text: colours.text, muted: colours.muted,
  line: colours.border, lineSoft: colours.border, accent: colours.accent, blue: colours.accent, red: colours.error,
  shortage: colours.error, amber: colours.warning, green: colours.positive, grey: colours.muted, hatch: colours.muted,
  hatchEdge: colours.border, font: 'Segoe UI, sans-serif', fontSize: { small: 11, body: 12 }, lineWidth: 1, hatchSpacing: 6,
};

function rig(kind: string, dpr: number, values = [30, 150]) {
  let now = 1000;
  let serial = 0;
  let presented: Presented<ImageBitmap> | undefined;
  const queued: (() => void)[] = [];
  const view = createLaneView(barsLane, () => {});
  const lane = new Lane<ImageBitmap>({
    lane: 0, lanes: 1, views: () => view, crop: async () => [], present: picture => { presented = picture; }, release() {},
    fail: reason => { throw new Error(reason); }, later: draw => queued.push(draw), clock: () => now,
  });
  const canvas = new OffscreenCanvas(1, 1);
  lane.setLaneSurface(canvas, true);
  lane.follow(1, 'panel', 'p');
  lane.bindRoute(1, 1, 9, 1, 0);
  lane.mount(1, [1], 'bars', {
    family: 'bars', kind, view: 'main', options: {}, sources: { main: 1 }, scrollTop: 0, width: 481, height: 235, theme,
  } satisfies FamilyBarsBinding);
  lane.bind(1, { generation: 1, pin: null, offset: 0, surfaces: [{ width: 481, height: 235, dpr }] });
  const layout = (width = 490, scrollLeft = 0, slotX = 3) => lane.setLaneSlots({
    width, height: 250, dpr, scrollLeft, scrollTop: 0, clipPathFallback: false, surfaceColor: theme.surface, cornerRadius: 9,
    slots: [{ widget: 1, x: slotX, y: 4, w: 481, h: 235, bx: slotX, by: 2, bw: 481, bh: 238, visible: true }],
  });
  layout();
  lane.receive('panels', datedPanel(1, 100, values[0]));
  lane.receive('panels', datedPanel(1, 101, values[1]));
  const request = async (now: number, speed = 0, acknowledge = false, refused: Refusal[] = []) => {
    const installed = acknowledge ? (presented?.entries ?? []).map(({ id, widget, binding }) => ({ id, widget, binding })) : [];
    lane.frame({ type: 'frame', serial: ++serial, now, target: now, exact: true, seek: 1, speed, installed, refused, inspect: [] });
    for (let i = 0; i < 12; i++) await Promise.resolve();
  };
  const clock = (playing: boolean, position = 100, speed = playing ? 1 : 0) => lane.setClock({
    playing, speed, anchorPosition: position, anchorTimeMs: performance.timeOrigin + now, seek: 1, exactHour: Math.floor(position),
  });
  const step = (elapsed = 200) => {
    now += elapsed;
    for (const draw of queued.splice(0)) draw();
  };
  return {
    request, clock, step, layout,
    pixels: (x = 0, y = 0, w = canvas.width, h = canvas.height) => canvas.getContext('2d')!.getImageData(x, y, w, h).data,
    later: () => lane.receive('panels', datedPanel(1, 104, 185)),
    revoke: () => lane.receive('panels', datedRecord(1, 101, 0, 'Withdrawn')),
    lastEntry: () => presented?.entries.at(-1),
  };
}

function changedBytes(a: Uint8ClampedArray, b: Uint8ClampedArray): number {
  let changed = 0;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) changed++;
  return changed;
}

function checkSubpixelHatchTail() {
  return [1, 1.25, 1.5, 2].flatMap(dpr => [false, true].map(moving => {
    const canvas = new OffscreenCanvas(16 * dpr, 16 * dpr);
    const ctx = canvas.getContext('2d')!;
    ctx.scale(dpr, dpr);
    ctx.fillStyle = theme.surface;
    ctx.fillRect(0, 0, 16, 16);
    const firstOutside = 12 * dpr;
    const column = (x: number) => ctx.getImageData(x, 0, 1, canvas.height).data;
    const background = column(firstOutside);
    drawSplitBar(ctx, 4, 2, 8, 12, [
      { fraction: 31 / 32, fill: theme.surface },
      { fraction: 1 / 32, fill: theme.surface, hatched: true },
    ], { ...theme, hatch: '#fff', hatchEdge: '#fff' }, moving ? { widths: [7.75, 0.25] } : undefined);
    return { dpr, moving, inside: changedBytes(background, column(firstOutside - 1)), outside: changedBytes(background, column(firstOutside)) };
  }));
}

async function checkReadoutClearance() {
  const sprite = new OffscreenCanvas(16, 16);
  const brush = sprite.getContext('2d')!;
  brush.fillStyle = theme.green;
  brush.fillRect(0, 0, 3, 16);
  const bitmap = sprite.transferToImageBitmap();
  const icon = { ...readyArt('fixture-readout', { frame: defaultArtFrame(16, 16) }), bitmap };
  const kind = 'target-bars-readout-clearance';
  registerFamilyProjection({
    family: 'bars', kind,
    singleValueBars: () => 'target-markers',
    project(view): BarsModel {
      const value = Number(view.panel('main'));
      const target = value === 900 ? 1000 : 1;
      return {
        form: 'ordered', hour: view.hour, scaleMax: target, showAxis: false,
        rows: [{
          kind: 'data', key: 'fuel', label: knownValue('Fuel'), numeric: value, value: { value: knownValue(value) },
          target: { numeric: target, value: knownValue(target) },
          readout: { icon, primary: String(value), secondary: `/ ${target} a day`, ...(value === 900 ? { shortfall: '\u2212100' } : {}) },
        }],
      };
    },
  }, 'main');
  const width = 490;
  const fill = parseColor(readableColour(theme.accent, theme.surface))!;
  const readout = parseColor(theme.green)!;
  const live = rig(kind, 1, [900, 1]);
  const cold = rig(kind, 1, [900, 1]);
  const edges = () => {
    const pixels = live.pixels(0, 0, width, 250);
    let barRight = -1;
    let readoutLeft = width;
    for (let i = 0; i < pixels.length; i += 4) {
      const x = (i / 4) % width;
      if (pixels[i] === fill.r && pixels[i + 1] === fill.g && pixels[i + 2] === fill.b) barRight = Math.max(barRight, x);
      if (pixels[i] === readout.r && pixels[i + 1] === readout.g && pixels[i + 2] === readout.b) readoutLeft = Math.min(readoutLeft, x);
    }
    return { barRight, readoutLeft };
  };
  live.clock(true);
  await live.request(100, 1);
  live.step(0);
  const first = edges();
  await cold.request(100);
  const firstDifference = changedBytes(cold.pixels(), live.pixels());
  const readoutPixels = () => live.pixels(first.readoutLeft, 0, width - first.readoutLeft, 250);
  const heldReadout = readoutPixels();
  const samples = [.25, .5, .75, .99].map(fraction => {
    live.clock(true, 100 + fraction);
    live.step(0);
    return { fraction, ...edges(), readoutDifference: changedBytes(heldReadout, readoutPixels()) };
  });
  live.clock(true, 101);
  live.step(0);
  await cold.request(101);
  const result = { first, samples, last: edges(), firstDifference, lastDifference: changedBytes(cold.pixels(), live.pixels()) };
  bitmap.close();
  return result;
}

async function checkSettledPixels(form: BarsPlaybackForm) {
  const sprite = new OffscreenCanvas(37, 21);
  const brush = sprite.getContext('2d')!;
  brush.fillStyle = theme.red;
  brush.fillRect(0, 0, 37, 21);
  brush.fillStyle = theme.text;
  brush.fillRect(9, 0, 23, 11);
  const bitmap = sprite.transferToImageBitmap();
  const flag = { ...readyArt('fixture-flag', { frame: defaultArtFrame(37, 21) }), bitmap };
  const kind = `bars-pixel-settle-${form}`;
  registerFamilyProjection({
    family: 'bars', kind,
    singleValueBars: () => form,
    project(view): BarsModel {
      const modelForm = form === 'share' ? 'share' : 'ordered';
      const refused = view.refused('main');
      if (refused) return { form: modelForm, hour: view.hour, rows: [], empty: refused, emptyKind: 'unavailable' };
      const value = Number(view.panel('main'));
      const target = value <= 0.1 ? 0.1 : value + 40;
      return {
        form: modelForm, hour: view.hour, scaleMax: 200, showAxis: form !== 'share',
        rows: [{
          kind: 'data', key: 'fixture', label: knownValue('Fixture country'), numeric: value, value: { value: knownValue(value) },
          media: { kind: 'flag', art: flag, tag: 'FIX' },
          ...(form === 'target-markers' ? {
            target: { numeric: target, value: knownValue(target) },
            readout: { primary: String(value), secondary: `/ ${target} a day` },
          } : {}),
          ...(form === 'ordered-segments' ? {
            segments: [
              { key: 'solid', label: 'Solid', share: value < 100 ? .75 : .25, color: theme.blue, value: knownValue(value * .25) },
              { key: 'dashed', label: 'Dashed', share: .75, color: theme.red, hatched: true, value: knownValue(value * .75) },
            ],
          } : {}),
          ...(form === 'share' ? {
            readout: { primary: `${value} units` },
            segments: [
              { key: 'solid', label: 'Solid', share: value < 100 ? .45 : .6, color: theme.blue, value: knownValue(value) },
              { key: 'dashed', label: 'Dashed', share: value < 100 ? .35 : .4, color: theme.red, hatched: true, value: knownValue(value) },
            ],
          } : {}),
        }],
      };
    },
  }, 'main');
  const results: { dpr: number; moving: number; paused: number }[] = [];
  const tinyEndpoints: { dpr: number; first: number; last: number }[] = [];
  for (const dpr of [1, 1.25, 1.5, 2]) {
    const played = rig(kind, dpr);
    await played.request(100);
    const initial = played.pixels();
    played.clock(true);
    played.step();
    played.step();
    const moving = changedBytes(initial, played.pixels());
    played.clock(false);
    await played.request(100);
    played.step();
    const cold = rig(kind, dpr);
    await cold.request(100);
    results.push({ dpr, moving, paused: changedBytes(cold.pixels(), played.pixels()) });
    const tinyValues = form === 'target-markers' ? [[0.1, 0], [0, 0.1]]
      : form === 'ordered-segments' || form === 'share' ? [[0.1, 150], [150, 0.1]] : [];
    for (const values of tinyValues) {
      const live = rig(kind, dpr, values);
      const endpoint = rig(kind, dpr, values);
      live.clock(true);
      await live.request(100, 1);
      live.step(0);
      await endpoint.request(100);
      const first = changedBytes(endpoint.pixels(), live.pixels());
      live.step(500);
      live.step(500);
      await endpoint.request(101);
      const last = changedBytes(endpoint.pixels(), live.pixels());
      tinyEndpoints.push({ dpr, first, last });
    }
  }
  const histories = [];
  for (const revoked of [false, true]) {
    const cold = rig(kind, 1.25);
    if (revoked) cold.revoke();
    await cold.request(revoked ? 100 : 101);
    const expected = cold.pixels();
    for (const history of ['skip', 'midpoint', 'late-ack', 'speed-change'] as const) {
      const played = rig(kind, 1.25);
      played.clock(true);
      await played.request(100, 1);
      await played.request(100, 1, true);
      if (history !== 'skip') played.step(500);
      if (history === 'late-ack') await played.request(101, 1);
      if (revoked) played.revoke();
      if (history === 'late-ack') {
        const entry = played.lastEntry()!;
        await played.request(102, 1, !revoked, revoked ? [{ widget: 1, id: entry.id, reason: 'kept content' }] : []);
      }
      if (history === 'speed-change' && !revoked) {
        played.later();
        cold.later();
        played.clock(true, 104);
        played.step(0);
      }
      played.clock(true, 102, history === 'speed-change' ? 24 : 1);
      played.step(0);
      histories.push({ revoked, history, difference: changedBytes(expected, played.pixels()) });
    }
  }
  const geometry = rig(kind, 1);
  geometry.clock(true);
  await geometry.request(100, 1);
  geometry.step(500);
  const region = (x = 3) => geometry.pixels(x, 2, 481, 238);
  const moving = region();
  geometry.layout(490, 12);
  const afterScroll = changedBytes(moving, region());
  const positioned = rig(kind, 1);
  positioned.layout(525, 12, 14);
  positioned.clock(true);
  await positioned.request(100, 1);
  positioned.step(500);
  const expected = positioned.pixels(14, 2, 481, 238);
  geometry.layout(525, 12, 14);
  const afterResize = changedBytes(expected, region(14));
  geometry.step(0);
  const afterTick = changedBytes(expected, region(14));
  const refused = rig(kind, 1.25);
  refused.clock(true);
  await refused.request(100, 1);
  refused.step(500);
  refused.revoke();
  refused.clock(true, 101.5);
  refused.step(0);
  const coldRefusal = rig(kind, 1.25);
  coldRefusal.revoke();
  await coldRefusal.request(101);
  const afterRefusal = changedBytes(refused.pixels(), coldRefusal.pixels());
  bitmap.close();
  return { results, tinyEndpoints, histories, afterScroll, afterResize, afterTick, afterRefusal };
}

(window as unknown as { checkSettledPixels: typeof checkSettledPixels }).checkSettledPixels = checkSettledPixels;
(window as unknown as { checkReadoutClearance: typeof checkReadoutClearance }).checkReadoutClearance = checkReadoutClearance;
(window as unknown as { checkSubpixelHatchTail: typeof checkSubpixelHatchTail }).checkSubpixelHatchTail = checkSubpixelHatchTail;
