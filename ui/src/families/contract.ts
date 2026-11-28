import type {
  BoundedReadView,
  CompiledFollowSubscription,
  FamilyId,
} from './plan.ts';
import type { TailValidator } from './table/rowSource.ts';

export type {
  ArtHandle,
  ArtProvider,
  ArtResult,
  AuxProductSpec,
  BoundedReadView,
  BoundedSeries,
  CountryPolicy,
  DisplayedHour,
  EntityKind,
  FamilyId,
  FamilyPlanContext,
  FamilyViewportRequest,
  FieldStatus,
  IdentityProvider,
  IdentityResult,
  InstalledAction,
  InstalledActionTarget,
  InstalledPicture,
  InstalledTransform,
  SourceCoverage,
  SourceEntry,
  SourcePlan,
} from './plan.ts';
export {
  displayedHour,
  FutureDataError,
} from './boundedView.ts';

export type FamilySourceSubscription = CompiledFollowSubscription;

export interface FamilyTheme {
  surface: string;
  raised?: string;
  dark?: boolean;
  text: string;
  muted: string;
  line: string;
  lineSoft: string;
  hairline?: string;
  hairlineStrong?: string;
  bandDivider?: string;
  headerBand?: string;
  rowHover?: string;
  rowSelected?: string;
  childTint?: string;
  accent: string;
  blue: string;
  red: string;
  shortage?: string;
  amber: string;
  green: string;
  grey: string;
  barMuted?: string;
  barTrack?: string;
  gridLine?: string;
  zeroLine?: string;
  tailLine?: string;
  playhead?: string;
  crosshair?: string;
  dateTabBg?: string;
  dateTabText?: string;
  hoverTabBg?: string;
  hoverTabText?: string;
  hatch: string;
  hatchEdge?: string;
  font: string;
  fontSize: { small: number; body: number };
  lineWidth: number;
  hatchSpacing: number;
}

export type BarsPlaybackForm = true | 'target-markers' | 'ordered-segments' | 'share';

export interface FamilyProjection<TModel, TOptions> {
  readonly kind: string;
  readonly family: FamilyId;
  singleValueBars?(options: TOptions): BarsPlaybackForm | false;
  project(readView: BoundedReadView, options: TOptions): TModel;
  nextChange?(readView: BoundedReadView, options: TOptions): number;
  readonly validateTail?: TailValidator;
}

export interface MirrorCell {
  readonly column: string;
  readonly spoken: string;
}

export interface MirrorRow {
  readonly key: string;
  readonly name: string;
  readonly rowIndex: number;
  readonly rank?: number;
  readonly level?: number;
  readonly group?: boolean;
  readonly open?: boolean;
  readonly groupHeader?: boolean;
  readonly cells: readonly MirrorCell[];
}

export interface MirrorModel {
  readonly first: number;
  readonly total: number;
  readonly rows: readonly MirrorRow[];
  readonly columns?: readonly string[];
}
