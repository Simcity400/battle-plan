import type { ActiveRegions } from '../../generated/dashboard';
import type { FamilyTooltipPayload } from '../host/tooltips.ts';
import { REGION_ADD_KEY, type RegionRowRef, type RegionRowsField } from '../regions/regionRows.ts';
import type { Value } from '../value.ts';
import type { ValueFormat } from '../valueFormat.ts';
import { barsLabelText, type BarsDataRow, type BarsLegendItem, type BarsModel, type BarsReadout, type BarsRow, type BarsRowMedia, type BarsSegment } from './model.ts';

export interface ShareRowEntry {
  readonly key: string;
  readonly subject?: string;
  readonly label: Value<string>;
  readonly media?: BarsRowMedia;
  readonly group?: 'picked' | 'auto';
  readonly region?: number;
  readonly segments: readonly BarsSegment[];
  readonly segmentsIncomplete?: boolean;
  readonly countText?: string | BarsReadout;
  readonly missing?: Value;
  readonly format?: ValueFormat;
  readonly tooltip?: FamilyTooltipPayload;
  readonly meta?: unknown;
}

export interface ShareRowsInput {
  readonly hour: number;
  readonly picked?: readonly ShareRowEntry[];
  readonly automatic?: readonly ShareRowEntry[];
  readonly rows?: readonly ShareRowEntry[];
  readonly pickedHeading?: string;
  readonly automaticHeading?: string;
  readonly addRegionLabel?: string | null;
  readonly summaryText?: string;
  readonly legend?: readonly BarsLegendItem[];
  readonly unit?: string;
  readonly empty?: string;
  readonly emptyDetail?: string;
  readonly emptyKind?: 'empty' | 'unavailable';
  readonly notes?: readonly string[];
  readonly warnings?: readonly string[];
  readonly nextChangeAt?: number;
  readonly active?: ActiveRegions;
}

function toShareDataRow(entry: ShareRowEntry, group?: 'picked' | 'auto'): BarsDataRow {
  return {
    kind: 'data',
    key: entry.key,
    subject: entry.subject ?? entry.key,
    label: entry.label,
    ...(entry.media ? { media: entry.media } : {}),
    group: entry.group ?? group,
    ...(entry.region !== undefined ? { region: entry.region } : {}),
    segments: entry.segments,
    ...(entry.segmentsIncomplete ? { segmentsIncomplete: true } : {}),
    ...(entry.missing ? { value: { value: entry.missing, format: entry.format } } : {}),
    ...(entry.countText !== undefined ? { readout: typeof entry.countText === 'string' ? { primary: entry.countText } : entry.countText } : {}),
    ...(entry.tooltip ? { tooltip: entry.tooltip } : {}),
    ...(entry.meta !== undefined ? { meta: entry.meta } : {}),
  };
}

export function buildShareRowsModel(input: ShareRowsInput): BarsModel {
  const rows: BarsRow[] = [];
  if (input.picked?.length || input.automatic?.length) {
    if (input.picked?.length) {
      rows.push({ kind: 'section', key: '__section:picked', label: input.pickedHeading ?? 'Your regions', count: input.picked.length });
      for (const entry of input.picked) rows.push(toShareDataRow(entry, 'picked'));
    }
    if (input.automatic?.length) {
      rows.push({ kind: 'section', key: '__section:auto', label: input.automaticHeading ?? 'Most active', count: input.automatic.length });
      for (const entry of input.automatic) rows.push(toShareDataRow(entry, 'auto'));
    }
  } else if (input.rows?.length) {
    for (const entry of input.rows) rows.push(toShareDataRow(entry));
  }
  if (rows.length > 0 && input.addRegionLabel) {
    rows.push({ kind: 'add-region', key: REGION_ADD_KEY, label: input.addRegionLabel });
  }
  return {
    form: 'share',
    hour: input.hour,
    rows,
    showAxis: false,
    ...(input.summaryText !== undefined ? { summaryText: input.summaryText } : {}),
    ...(input.legend?.length ? { legend: input.legend } : {}),
    ...(input.unit ? { scaleUnit: input.unit } : {}),
    ...(input.empty !== undefined ? { empty: input.empty } : {}),
    ...(input.emptyDetail !== undefined ? { emptyDetail: input.emptyDetail } : {}),
    ...(input.emptyKind !== undefined ? { emptyKind: input.emptyKind } : {}),
    ...(input.notes ? { notes: input.notes } : {}),
    ...(input.warnings ? { warnings: input.warnings } : {}),
    ...(input.nextChangeAt !== undefined ? { nextChangeAt: input.nextChangeAt } : {}),
    ...(input.active ? { regionsActive: input.active } : {}),
  };
}

export function barsRegionRows(model: BarsModel, layout: { readonly bodyTop: number; readonly rowTops: readonly number[]; readonly rowHeights: readonly number[]; readonly nameX: number; readonly nameWidth: number }): RegionRowsField | null {
  const rows: RegionRowRef[] = [];
  let add: RegionRowsField['add'] = null;
  model.rows.forEach((row, index) => {
    const top = layout.bodyTop + layout.rowTops[index]!;
    const height = layout.rowHeights[index]!;
    if (row.kind === 'add-region') add = { top, height };
    if (row.kind !== 'data' || row.region === undefined) return;
    rows.push({
      key: row.key,
      id: row.region,
      label: barsLabelText(row.label),
      picked: row.group === 'picked',
      top,
      height,
      active: row.group !== 'picked' || (row.segments ?? []).some(segment => segment.share > 0),
    });
  });
  return rows.length || add || model.regionsActive ? { rows, add, nameLeft: layout.nameX, nameRight: layout.nameX + layout.nameWidth, ...(model.regionsActive ? { active: model.regionsActive } : {}) } : null;
}
