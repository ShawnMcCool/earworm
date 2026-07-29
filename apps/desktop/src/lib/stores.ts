// All UI state derives from dispatch responses + events — no second source
// of truth. Stores mirror the wire shapes of `server::app::App`.

import { derived, get, writable } from "svelte/store";
import { cmd, initialSong, onEvent } from "./ipc";
import { trace, traceErr } from "./trace";
import { subdivisionTimes, type GridSubdivision } from "./waveform-math";
import { bisect, nudgeEdge, rateForRep, runUp, type Span } from "./drill";
import { setActiveIn, setCollapsed, type RegionId, type Workspace } from "./dock";
import { migrateWorkspace } from "./workspace-migrate";
import { defaultFlow } from "./stage";
import { deriveLoopName } from "./loop-name";
import { isolationToStemMix, stemMixToIsolation, type Isolation } from "./isolation";
import { meterNumerator } from "./meter";
import { tapTempo as computeTap, clampBpm, strongMask, type TapState } from "./metronome";
import { resolveInputDevice } from "./devices";
import type { NotesDoc } from "./notes-doc";
import { framesToMs } from "./recording-math";

// --- wire types ----------------------------------------------------------

export interface Song {
  id: number;
  title: string;
  artist: string | null;
  path: string;
  file_hash: string;
  duration_secs: number;
}

export interface Section {
  id: number;
  song_id: number;
  name: string;
  start: number;
  end: number;
  position: number;
  /** Occurrence label ("verse 2") — present on open-song payloads. */
  label?: string;
  /** Stored notes for this section, if any. */
  notes?: NotesDoc | null;
  /** Per-section beat-click guide flag (wire field, snake_case from the server). */
  click_guide?: boolean;
}

export interface OrphanNote {
  label: string;
  doc: NotesDoc;
}

export type LoopKind =
  | { kind: "manual" }
  | { kind: "junction"; from_section: number; to_section: number };

export interface LoopRegion {
  id: number;
  song_id: number;
  name: string;
  /** Manual name pinned by the user; null when `name` is algorithm-derived. */
  name_override: string | null;
  start: number;
  end: number;
  kind: LoopKind;
}

// ── Practice routines (mirrors practice::model) ───────────────────────────────

/** Resolved isolation state: bass-focus + per-stem gains (vocals/drums/bass/
 *  other, 0..1). The canonical "what you hear", matching the backend `Mix`. */
export interface Mix {
  bass_focus: boolean;
  stems: number[];
}

export interface CountIn {
  beats: number;
  loop_mode: "first" | "every";
}

export interface Block {
  span: { start: number; end: number };
  mix: Mix;
  speed: number;
  passes: number;
  lead_in_beats: number;
  count_in: CountIn;
  name: string | null;
}

export interface Routine {
  id: number;
  name: string;
  blocks: Block[];
}

/** Pushed by the backend scheduler each time the active block changes. */
export interface RoutineStatus {
  running: boolean;
  routine_id: number;
  block_index: number;
  block_count: number;
  passes_remaining: number;
  block: Block;
}

export type TempoCurve =
  | { curve: "dwell"; rate: number }
  | { curve: "ladder"; start: number; step: number; target: number }
  | { curve: "oscillate"; low: number; high: number; period: number };

export interface Peaks {
  frames_per_bucket: number;
  buckets: [number, number][];
}

/** One suggested section from analysis — not user truth until saved. */
export interface AnalysisSection {
  label: string;
  start: number;
  end: number;
}

/** Cached `scripts/analyze` output for a song (times in seconds). */
export interface Analysis {
  bpm: number | null;
  beats: number[];
  downbeats: number[];
  sections: AnalysisSection[];
  engine: string;
}

export interface ProfileStage {
  name: string;
  ms: number;
  note?: string;
}

export interface ProfileRun {
  op: string;
  song_id?: number;
  started_at: string;
  total_ms: number;
  ok: boolean;
  error?: string;
  device?: string;
  engine?: string;
  stages: ProfileStage[];
  max_cpu_pct?: number;
  max_gpu_util?: number;
  max_vram_used_mb?: number;
  vram_total_mb?: number;
}

export interface WorkSample {
  op: string;
  stage: string;
  elapsed_ms: number;
  cpu_pct: number;
  gpu_util?: number;
  gpu_mem_used_mb?: number;
  gpu_mem_total_mb?: number;
  ram_used_mb?: number;
  ram_total_mb?: number;
}

/** A saved playhead marker slot (mirrors `practice::model::Marker`). */
export interface Marker {
  slot: number;
  pos: number;
}

/** A saved isolation-mix snapshot slot (mirrors `practice::model::IsolationSnapshot`). */
export interface IsolationSnapshot {
  slot: number;
  name: string | null;
  state: Isolation;
}

/** One row of the global pedal mapping (the `pedal_mapping` setting). */
export interface PedalBinding {
  trigger: string;
  action: string;
  slot?: number;
}

export interface OpenSong {
  song: Song;
  sections: Section[];
  loops: LoopRegion[];
  routines: Routine[];
  peaks: Peaks;
  /** True when the engine was loaded with the song's 4 cached stems. */
  stems: boolean;
  analysis: Analysis | null;
  /** Notes whose label matches no current section. Never auto-deleted. */
  orphan_notes: OrphanNote[];
  recordings: Recording[];
  /** Saved isolation-box state (bass focus + per-stem levels/mutes/solos). */
  isolation: Isolation;
  /** Saved playhead marker slots. */
  markers: Marker[];
  /** Saved isolation-mix snapshot slots. */
  snapshots: IsolationSnapshot[];
}

/** Fixed stem order contract — mirrors `practice::model::STEM_NAMES`:
 *  vocals/drums/bass/guitar/piano/other (the demucs 6-stem vocabulary). */
export const STEM_LABELS = ["VOCALS", "DRUMS", "BASS", "GUITAR", "PIANO", "OTHER"] as const;
export const BASS_STEM = 2;

export interface Recording {
  id: number;
  name: string;
  file: string;
  anchor_frame: number;
  len_frames: number;
  nudge_frames: number;
  gain: number;
  muted: boolean;
  created_at: string;
  /** Waveform peaks for this take's audio, or null if it failed to decode.
   *  A recomputable cache computed server-side — not stored in the manifest. */
  peaks: Peaks | null;
}

export interface CalibrationResult {
  latency_frames: number;
  latency_ms: number;
  source: string;
  envelope: number[];
  emit_index: number;
  onset_index: number;
  window_ms: number;
}

export interface LatencyStatus {
  auto_frames: number | null;
  loopback_frames: number | null;
  source: string;
}

export interface StemMix {
  levels: number[]; // 0..100 per stem
  mutes: boolean[];
  solos: boolean[];
}

function defaultStemMix(): StemMix {
  const n = STEM_LABELS.length;
  return {
    levels: Array(n).fill(100),
    mutes: Array(n).fill(false),
    solos: Array(n).fill(false),
  };
}

export interface Position {
  secs: number;
  rate: number;
  playing: boolean;
  /** performance.now() at receipt — playhead extrapolation anchor. */
  at: number;
  /** Set while the count-in pre-roll is sounding: the playhead is held at
   *  `secs` and pulses on each beat. `null` during normal playback. */
  countIn: { beat: number; of: number } | null;
}

export interface TunerReading {
  hz: number;
  /** McLeod clarity 0..1; < 0.5 means no steady pitch. */
  confidence: number;
}

// --- stores ---------------------------------------------------------------

export const songs = writable<Song[]>([]);
export const openSong = writable<OpenSong | null>(null);
/** Song id with a `song.open` in flight — drives the library row spinner and
 *  the stage loading state; null once the open settles (also on error). */
export const openingSong = writable<number | null>(null);
export const position = writable<Position>({
  secs: 0,
  rate: 1,
  playing: false,
  at: 0,
  countIn: null,
});
export const selection = writable<{ start: number; end: number } | null>(null);
/** Loop currently driving the transport (clicked or plan-applied). */
export const currentLoop = writable<LoopRegion | null>(null);
/** A live, unsaved loop: drag a selection, hit "loop", and it plays + drills
 *  exactly like a saved loop — but nothing is persisted until you save it. At
 *  most one exists; a fresh "loop" silently replaces it, clicking away dismisses
 *  it. Stays null while a saved loop is active (the two are mutually exclusive). */
export const workingLoop = writable<Span | null>(null);
/** The loop currently driving the stage — a working loop if one is up, else the
 *  selected saved loop — normalized so the drill box, waveform and transport all
 *  read one shape. `id === null` marks a working (unsaved) loop. The working
 *  loop's name is derived from the song's sections exactly as the server names
 *  saved loops (e.g. "verse 2", "verse 2 → chorus 1"), so it reads like any
 *  other loop — there's no user-facing "working loop" concept. */
export const activeLoop = derived(
  [workingLoop, currentLoop, openSong],
  ([$working, $current, $open]): { id: number | null; start: number; end: number; name: string } | null =>
    $working
      ? {
          id: null,
          start: $working.start,
          end: $working.end,
          name: deriveLoopName($working.start, $working.end, $open?.sections ?? []),
        }
      : $current
        ? { id: $current.id, start: $current.start, end: $current.end, name: $current.name }
        : null,
);
/** Ephemeral scratch loop bounds for the drill box — what actually plays while
 *  a loop is active. Mirrors the active loop's bounds, then the drill region
 *  toys (nudge / isolate / run-up) edit *this* only; the saved LoopRegion is
 *  never touched. Null when no loop is active. */
export const drillSpan = writable<Span | null>(null);
/** The bounds the scratch span resets to — the "home" of the current drill,
 *  seeded whenever a loop is engaged (a saved loop, or a transient
 *  selection-loop). Null when no loop is active; the drill box shows iff this
 *  (and drillSpan) is non-null. */
export const drillHome = writable<Span | null>(null);
// The seeding/teardown is centralized in `actions.seedDrill`, driven by the
// active loop (working or saved): engaging a loop seeds it, clearing/reset tears
// it down. The activeLoop lifecycle hook is set up after `actions` is defined
// (see "drill box lifecycle" near the bottom).

/** Step-up tempo trainer for the drill box: a ramp recipe (a `TempoCurve`) that
 *  autopilots the *global* playback rate across loop cycles. No second tempo —
 *  it animates `position.rate`. `cycle` is the 0-based loop-wrap count since
 *  arming. The recipe persists across loops; arming resets the cycle. */
export interface DrillTrainer {
  recipe: TempoCurve;
  armed: boolean;
  cycle: number;
}
export const drillTrainer = writable<DrillTrainer>({
  recipe: { curve: "ladder", start: 0.7, step: 0.05, target: 1.0 },
  armed: false,
  cycle: 0,
});

/** Mute-to-recall for the drill box: play a pass "from memory" by muting the
 *  recording (the engine keeps the position advancing, so the loop stays in
 *  time). `armNext` silences the next pass; `everyN` silences every Nth. Recall
 *  drives the engine mute only while active, and restores audio when cleared. */
export const drillRecall = writable<{ everyN: number | null; armNext: boolean }>({
  everyN: null,
  armNext: false,
});

/** Which drill tools the user has opted into for the active loop. The box is
 *  minimal by default — a fresh loop just plays; tools are added on demand and
 *  reset (back to minimal) by seedDrill whenever the loop changes. */
export const drillShow = writable({ trainer: false, recall: false, region: false });
/** Bumped by `resetWorkspace()` — the waveform watches this to refit zoom and
 *  drop its local view/active-span state (which no store mirrors). */
export const workspaceReset = writable(0);
/** Snapshot slot last applied server-side; cleared by any manual fader edit. */
export const activeSnapshotSlot = writable<number | null>(null);
/** Last normalized MIDI trigger seen (learn flow); `seq` forces reactivity on
 *  repeated identical triggers. */
export const lastMidiTrigger = writable<{ trigger: string; seq: number } | null>(null);
export const pitch = writable({ semitones: 0, cents: 0, octaveUp: false });
export const countIn = writable<{ enabled: boolean; beats: number; loopMode: "first" | "every" }>({
  enabled: true,
  beats: 4,
  loopMode: "first",
});
/** Count-in needs a tempo, so it only applies once the song has analysis. */
export const countInAvailable = derived(openSong, ($o) => $o?.analysis?.bpm != null);
/** Section-click master arm — gates whether per-section click guides sound. */
export const sectionClick = writable<{ enabled: boolean }>({ enabled: false });
/** Section click needs an analyzed beat grid — same gate as count-in. */
export const sectionClickAvailable = countInAvailable;

export type Cadence = "beat" | "half" | "bar";
export type Kit = "click" | "kick_snare" | "cowbell";

export interface MetronomeState {
  running: boolean;
  bpm: number;
  beatsPerBar: number;
  cadence: Cadence;
  kit: Kit;
}

export const metronome = writable<MetronomeState>({
  running: false,
  bpm: 120,
  beatsPerBar: 4,
  cadence: "beat",
  kit: "click",
});

/** Live beat from the engine: 1-based beat in the bar, or null when stopped. */
export const metronomeBeat = writable<{ beat: number; of: number; sounded: boolean } | null>(null);

/** Bass focus on/off — low-pass + octave-up transcription trick. */
export const bassFocus = writable(false);
export const muted = writable(false);
/** User playback volume 0..1.5 — engine multiplier, persisted as a setting. */
export const playbackVolume = writable(1.0);
export interface AudioDevice { id: string; name: string; is_default: boolean }
/** Known output devices (audio sinks). Refreshed on the devices tab. */
export const outputDevices = writable<AudioDevice[]>([]);
/** Persisted output device id; null means follow the system default. */
export const outputDevice = writable<string | null>(null);
/** Known input devices (audio sources). Refreshed on the devices tab. */
export const inputDevices = writable<AudioDevice[]>([]);
/** Persisted input device id; null means follow the system default. */
export const inputDevice = writable<string | null>(null);

/** Latest pitch reading while the tuner is on; null when off. */
export const tunerReading = writable<TunerReading | null>(null);
/** Whether the tuner box is powered on (listening). */
export const tunerOn = writable(false);
/** Tuner input selection: an `AudioDevice.id`, or the sentinel `"default"`
 *  meaning "follow the global input device". Restored from settings at launch. */
export const tunerInput = writable<string>("default");
/** Mixer state for the open song's stems (sliders × mute × solo). */
export const stemMix = writable<StemMix>(defaultStemMix());

/** The running practice routine, or null. Set from the `routine` event and the
 *  start/stop responses; drives the block indicator and the fader animation. */
export const activeRoutine = writable<RoutineStatus | null>(null);
export const stemsError = writable<string | null>(null);
/** All overdub recordings for the open song. */
export const recordings = writable<Recording[]>([]);
/** True while a recording.start has been sent and recording.stop has not yet resolved. */
export const recordingActive = writable<boolean>(false);
/** Record-arming (DAW-style): the recordings box configures the take and arms;
 *  the transport's record button triggers it. */
export type RecordSpan = "song" | "selection" | "loop" | "playhead";
export const recordArmed = writable<boolean>(false);
/** The armed span; "playhead" (the default) records from the playhead to the
 *  song end (mapped to a selection span for the backend). */
export const recordSpan = writable<RecordSpan>("playhead");
/** Armed input: an `AudioDevice.id`, or "default" = follow the devices panel. */
export const recordInput = writable<string>("default");
/** Live input level (peak/RMS, linear 0..~1) from the record-arming monitor,
 *  or null when the monitor is off. Drives the arming meter so a silent/dead
 *  input is visible before a take is committed. */
export const inputLevel = writable<{ peak: number; rms: number } | null>(null);
/** Recording-latency status for the devices readout (both measurements + which is active). */
export const latencyStatus = writable<LatencyStatus | null>(null);
export const analysisError = writable<string | null>(null);
/** Loop edges snap to downbeats while on (only meaningful with analysis). */
export const gridSnap = writable(true);
/** Single clicks on the waveform lock to the nearest grid target while grid
 *  snap is on (persisted). Off = clicks seek exactly where they land. */
export const clickSnap = writable(false);
/** Active-play (persisted): when on, a click that places the playhead — a
 *  section header, a spot on the wave — also starts playback. Off = the click
 *  only moves the playhead. */
export const activePlay = writable(false);
/** Every tab (page) in the workspace, in code order. `library` is the first —
 *  `defaultWorkspace` seeds it into the left region, the rest into the right.
 *  The view map lives in App.svelte (`TAB_VIEWS`). */
export const ALL_TABS = [
  "library",
  "structure",
  "loops",
  "routines",
  "export",
  "profile",
  "devices",
  "pedal",
  "settings",
  "guide",
] as const;

/** The window arrangement: left + right regions, each a dock stack plus its
 *  collapse flag. One source of truth — supersedes the old `panel_layout`,
 *  `library_collapsed`, and `panels_collapsed` settings (migrated on load by
 *  `migrateWorkspace`, then no longer written). */
export const workspace = writable<Workspace>({
  left: { layout: [], collapsed: false },
  right: { layout: [], collapsed: false },
  stage: defaultFlow(),
});
/** Grid display (persisted): show/hide the drawn grid, full lines vs bottom
 *  ticks, and the subdivision used for both the grid and snapping. */
export const gridVisible = writable(true);
export const gridLines = writable(false);
export const gridSubdivision = writable<GridSubdivision>("bar");
/** Show every saved loop on the waveform (persisted). Off by default — the
 *  waveform draws only the active loop; this brings the rest back as a dim
 *  overlay. The full list is always available in the loops tab regardless. */
export const allLoopsVisible = writable(false);
/** Recent profiling runs, most-recent-first. Mirrors `profile_run` events
 *  plus a `profiles.list` fetch at launch. */
export const profiles = writable<ProfileRun[]>([]);
/** Latest live work sample while a prepare run is active; null when idle. */
export const workSample = writable<WorkSample | null>(null);
/** VRAM series for the active run: used-MB samples (rolling last 60), the run's
 *  peak used-MB (high-water mark), and total VRAM. Null when idle / no GPU. */
export const vram = writable<{ used: number[]; peak: number; min: number; total: number } | null>(null);

// --- durable settings -------------------------------------------------------

/** Known keys in the server-side `settings` table. */
export const UI_SCALE = "ui_scale";
export const GRID_SNAP_DEFAULT = "grid_snap_default";
export const CLICK_SNAP = "click_snap";
export const ACTIVE_PLAY = "active_play";
/** The whole window arrangement (left + right regions). Supersedes
 *  `panel_layout` / `library_collapsed` / `panels_collapsed`, which are read
 *  once by `migrateWorkspace` and never written again. */
export const WORKSPACE = "workspace";
// legacy keys — read only by the one-time migration in `migrateWorkspace`:
//   panel_layout, library_collapsed, panels_collapsed, tab_order
export const PLAYBACK_VOLUME = "playback_volume";
export const ANALYSIS_DEVICE = "analysis_device";
export const GRID_VISIBLE = "grid_visible";
export const GRID_LINES = "grid_lines";
export const GRID_SUBDIV = "grid_subdivision";
export const ALL_LOOPS_VISIBLE = "all_loops_visible";
/** Native window frame (title bar + min/max/close). Default on. */
export const WINDOW_DECORATIONS = "window_decorations";
/** Accent colour theme: "amber" (default) or "cyan". */
export const COLOR_THEME = "color_theme";
/** Tuner input selection: an `AudioDevice.id`, or `"default"` (follow global). */
export const TUNER_INPUT = "tuner_input";
/** Export tab: last-used target folder + format, restored across sessions. */
export const EXPORT_DIR = "export_dir";
export const EXPORT_FORMAT = "export_format";
/** Where song bundles live. Empty = the OS default (`<music dir>/dredge`).
 *  Applies on restart. */
export const LIBRARY_ROOT = "library_root";
/** Chosen audio output device id; empty string means system default. */
export const OUTPUT_DEVICE = "output_device";
/** Chosen audio input device id; empty string means system default. */
export const INPUT_DEVICE = "input_device";
/** Count-in config: `{ beats, loopMode }`. beats 0 = off. Persisted. */
export const COUNT_IN = "count_in";
/** Section-click master arm: `{ enabled }`. Persisted. */
export const SECTION_CLICK = "section_click";
/** Metronome config: `{ bpm, beats_per_bar, cadence, kit }`. Persisted. */
export const METRONOME = "metronome";
/** Global foot-pedal → action mapping: `PedalBinding[]`. Persisted. */
export const PEDAL_MAPPING = "pedal_mapping";

/** Local mirror of the settings table; `loadSettings` fills it at launch and
 *  `setSetting` writes through. */
export const settings = writable<Record<string, unknown>>({});
/** Settings modal visibility (gear button or `,`). */
export const settingsOpen = writable(false);
/** Request: jump to the sections tab (e.g. from the structure summary). */
export const sectionsOpen = writable(false);
/** Request: jump to the loops tab (e.g. after saving a loop). */
export const loopsOpen = writable(false);

// --- prepare flow -----------------------------------------------------------

export type PrepareStepState =
  | "pending"
  | "running"
  | "done"
  | "failed"
  | "cached"
  | "cancelled";

export interface PrepareState {
  open: boolean;
  song_id: number;
  steps: { analysis: PrepareStepState; stems: PrepareStepState };
  errors: { analysis?: string; stems?: string };
}

/** Progress-modal state machine for `prepare()`; null when idle. */
export const prepareState = writable<PrepareState | null>(null);

type PrepareStep = "analysis" | "stems";
type ProgressReport = { song_id: number; state: string; error?: string };

/** `prepare()` awaits these; the matching `*_progress` event resolves one and
 *  is then handled by prepare's own refresh instead of the default branch. */
const prepareWaiters: Partial<Record<PrepareStep, (r: ProgressReport) => void>> = {};
let prepareSongId: number | null = null;
/** Set by `cancelPrepare()`; `prepare()` reads it to skip later steps. */
let prepareCancelled = false;

function setPrepareStep(step: PrepareStep, state: PrepareStepState, error?: string): void {
  prepareState.update((s) =>
    s
      ? {
          ...s,
          steps: { ...s.steps, [step]: state },
          errors: error ? { ...s.errors, [step]: error } : s.errors,
        }
      : s,
  );
}

/** A `*_progress` event for the song being prepared — claims the waiter. */
function takePrepareWaiter(
  step: PrepareStep,
  data: ProgressReport,
): ((r: ProgressReport) => void) | null {
  const waiter = prepareWaiters[step];
  if (!waiter || data.song_id !== prepareSongId) return null;
  delete prepareWaiters[step];
  return waiter;
}

/** Drill-box recall bookkeeping: loop-wrap counter and whether the *recall*
 *  feature (not the user's speaker toggle) currently owns the engine mute. */
let drillPass = 0;
let recallMuted = false;

/** Debounce handle for the volume fader's settings write-through. */
let volumeSaveTimer: ReturnType<typeof setTimeout> | undefined;
let isolationSaveTimer: ReturnType<typeof setTimeout> | undefined;

/** Tap-tempo accumulator for the metronome's tap action. */
let tapState: TapState = { taps: [] };

/** The active loop's bounds the drill was last seeded from — the seeder (set up
 *  near the bottom) compares against this so a save-promotion or an in-place
 *  resize of the *same* loop doesn't tear down the drill. */
let lastDrillBounds: string | null = null;

function loopName(id: number): string {
  return get(openSong)?.loops.find((l) => l.id === id)?.name ?? `loop ${id}`;
}

export { loopName };

// --- actions ----------------------------------------------------------------

export const actions = {
  // --- settings ---

  /** Pull the durable settings once at launch and apply the ones that act
   *  as session defaults (grid snap, playback volume). */
  async loadSettings(): Promise<void> {
    const all = await cmd<Record<string, unknown>>("settings.get_all");
    settings.set(all);
    if (typeof all[GRID_SNAP_DEFAULT] === "boolean") gridSnap.set(all[GRID_SNAP_DEFAULT]);
    if (typeof all[CLICK_SNAP] === "boolean") clickSnap.set(all[CLICK_SNAP]);
    if (typeof all[ACTIVE_PLAY] === "boolean") activePlay.set(all[ACTIVE_PLAY]);
    // The whole window arrangement: an existing `workspace` wins; else migrate
    // the legacy panel_layout / *_collapsed keys. Reconciled against ALL_TABS.
    workspace.set(migrateWorkspace(all, [...ALL_TABS]));
    if (typeof all[GRID_VISIBLE] === "boolean") gridVisible.set(all[GRID_VISIBLE]);
    if (typeof all[GRID_LINES] === "boolean") gridLines.set(all[GRID_LINES]);
    if (all[GRID_SUBDIV] === "bar" || all[GRID_SUBDIV] === "beat" || all[GRID_SUBDIV] === "eighth")
      gridSubdivision.set(all[GRID_SUBDIV]);
    if (typeof all[ALL_LOOPS_VISIBLE] === "boolean") allLoopsVisible.set(all[ALL_LOOPS_VISIBLE]);
    const vol = typeof all[PLAYBACK_VOLUME] === "number" ? all[PLAYBACK_VOLUME] : 1.0;
    playbackVolume.set(vol);
    await cmd("volume", { value: vol });
    if (typeof all[TUNER_INPUT] === "string" && all[TUNER_INPUT]) tunerInput.set(all[TUNER_INPUT]);
    const ci = all[COUNT_IN];
    if (ci && typeof ci === "object") {
      const c = ci as { enabled?: unknown; beats?: unknown; loop_mode?: unknown };
      // Migrate the old shape where beats 0 meant off (no `enabled` key).
      const rawBeats = typeof c.beats === "number" ? c.beats : 4;
      countIn.set({
        enabled: typeof c.enabled === "boolean" ? c.enabled : rawBeats > 0,
        beats: rawBeats > 0 ? rawBeats : 4,
        loopMode: c.loop_mode === "every" ? "every" : "first",
      });
    }
    const sc = all[SECTION_CLICK];
    if (sc && typeof sc === "object") {
      const s = sc as { enabled?: unknown };
      sectionClick.set({ enabled: typeof s.enabled === "boolean" ? s.enabled : false });
    }
    const mt = all[METRONOME];
    if (mt && typeof mt === "object") {
      const m = mt as Partial<MetronomeState> & { beats_per_bar?: number };
      metronome.update((s) => ({
        ...s,
        bpm: typeof m.bpm === "number" ? clampBpm(m.bpm) : s.bpm,
        beatsPerBar: typeof m.beats_per_bar === "number" ? m.beats_per_bar : s.beatsPerBar,
        cadence: (m.cadence as Cadence) ?? s.cadence,
        kit: (m.kit as Kit) ?? s.kit,
        running: false,
      }));
    }
    const od = all[OUTPUT_DEVICE];
    outputDevice.set(typeof od === "string" && od ? od : null);
    const idv = all[INPUT_DEVICE];
    inputDevice.set(typeof idv === "string" && idv ? idv : null);
    void actions.loadProfiles();
  },

  /** Pull recent profiling runs (most-recent-first) at launch. */
  async loadProfiles(): Promise<void> {
    profiles.set(await cmd<ProfileRun[]>("profiles.list", { limit: 50 }));
  },

  /** Prepend a freshly finished run (from a `profile_run` event). */
  recordProfile(run: ProfileRun): void {
    profiles.update((list) => [run, ...list].slice(0, 100));
  },

  /** Store the latest live work sample (from a `work_sample` event). */
  recordWorkSample(sample: WorkSample): void {
    workSample.set(sample);
    if (sample.gpu_mem_used_mb != null && sample.gpu_mem_total_mb != null) {
      const used = sample.gpu_mem_used_mb;
      const total = sample.gpu_mem_total_mb;
      vram.update((v) => ({
        used: [...(v?.used ?? []), used].slice(-60),
        peak: Math.max(v?.peak ?? 0, used),
        min: v?.min != null ? Math.min(v.min, used) : used,
        total,
      }));
    }
  },

  /** Write-through: update the local mirror, persist server-side. */
  async setSetting(key: string, value: unknown): Promise<void> {
    settings.update((s) => ({ ...s, [key]: value }));
    await cmd("settings.set", { key, value });
  },

  /** Persist the whole window arrangement (write-through). */
  async setWorkspace(ws: Workspace): Promise<void> {
    workspace.set(ws);
    await this.setSetting(WORKSPACE, ws);
  },

  /** Toggle one region's collapse flag. */
  async toggleRegion(region: RegionId): Promise<void> {
    const ws = get(workspace);
    await this.setWorkspace(setCollapsed(ws, region, !ws[region].collapsed));
  },

  /** Bring `tab` to the front of its region and un-collapse that region — the
   *  one canonical "reveal a page" path (shortcuts + the waveform's library
   *  link both route through here). No-op if the tab lives nowhere. */
  async revealTab(tab: string): Promise<void> {
    const ws = get(workspace);
    const region: RegionId | null = ws.left.layout.some((p) => p.tabs.includes(tab))
      ? "left"
      : ws.right.layout.some((p) => p.tabs.includes(tab))
        ? "right"
        : null;
    if (!region) return;
    const panel = ws[region].layout.findIndex((p) => p.tabs.includes(tab));
    let next = setActiveIn(ws, region, panel, tab);
    if (next[region].collapsed) next = setCollapsed(next, region, false);
    await this.setWorkspace(next);
  },

  async setGridSnap(on: boolean): Promise<void> {
    gridSnap.set(on);
    await this.setSetting(GRID_SNAP_DEFAULT, on);
  },
  async setClickSnap(on: boolean): Promise<void> {
    clickSnap.set(on);
    await this.setSetting(CLICK_SNAP, on);
  },
  async setActivePlay(on: boolean): Promise<void> {
    activePlay.set(on);
    await this.setSetting(ACTIVE_PLAY, on);
  },
  async setGridVisible(on: boolean): Promise<void> {
    gridVisible.set(on);
    await this.setSetting(GRID_VISIBLE, on);
  },
  async setGridLines(on: boolean): Promise<void> {
    gridLines.set(on);
    await this.setSetting(GRID_LINES, on);
  },
  async setGridSubdivision(sub: GridSubdivision): Promise<void> {
    gridSubdivision.set(sub);
    await this.setSetting(GRID_SUBDIV, sub);
  },
  async setAllLoopsVisible(on: boolean): Promise<void> {
    allLoopsVisible.set(on);
    await this.setSetting(ALL_LOOPS_VISIBLE, on);
  },

  async refreshSongs(): Promise<void> {
    songs.set(await cmd<Song[]>("song.list"));
  },

  async importSong(path: string): Promise<Song> {
    const song = await cmd<Song>("song.import", { path });
    await this.refreshSongs();
    await this.openSong(song.id);
    return song;
  },

  async deleteSong(id: number): Promise<void> {
    await cmd("song.delete", { song_id: id });
    if (get(openSong)?.song.id === id) openSong.set(null);
    await this.refreshSongs();
  },

  async updateSong(id: number, title: string, artist: string | null): Promise<void> {
    const song = await cmd<Song>("song.update", { song_id: id, title, artist });
    openSong.update((o) => (o && o.song.id === id ? { ...o, song } : o));
    await this.refreshSongs();
  },

  async reanalyze(): Promise<void> {
    const open = get(openSong);
    if (!open) return;
    // the analysis_progress event handler reloads the open song's analysis
    await cmd("analysis.run", { song_id: open.song.id, force: true });
  },

  async openSong(id: number): Promise<void> {
    // Phase tracing: a stuck spinner means this flow never reached `finally`.
    // Each milestone is forwarded to dredge.log, so the LAST line logged tells
    // us exactly which step the open froze on (network/backend, or the reactive
    // waveform render after `openSong.set`).
    trace("open", `#${id} begin`);
    openingSong.set(id);
    try {
      const data = await cmd<OpenSong>("song.open", { song_id: id });
      trace("open", `#${id} song.open ok — ${data.peaks?.buckets?.length ?? "?"} buckets`);
      localStorage.setItem(LAST_SONG_KEY, String(id));
      openSong.set(data);
      trace("open", `#${id} openSong store set (waveform render scheduled) — open complete`);
      selection.set(null);
      currentLoop.set(null);
      workingLoop.set(null);
      // Restore the song's saved isolation state, then push it to the engine.
      // Stem gains only mean anything once stems are loaded; bass focus applies
      // either way and carries its octave-up pitch trick with it. Setting these
      // here also clears any leak from the prior song (bassFocus / octaveUp were
      // never reset on open before).
      const focus = data.isolation.bass_focus;
      stemMix.set(isolationToStemMix(data.isolation));
      bassFocus.set(focus);
      const p = get(pitch);
      pitch.set({ ...p, octaveUp: focus });
      if (data.stems) void cmd("stems.gains", { gains: actions.stemGainsVector(get(stemMix)) });
      void cmd("bass_focus", { on: focus });
      void cmd("pitch", { semitones: p.semitones, cents: p.cents, octave_up: focus });
      activeRoutine.set(null);
      activeSnapshotSlot.set(null);
      stemsError.set(null);
      analysisError.set(null);
      recordings.set(data.recordings ?? []);
      recordingActive.set(false);
    } finally {
      openingSong.set(null);
      trace("open", `#${id} spinner cleared`);
    }
  },

  async refreshLoops(): Promise<void> {
    const open = get(openSong);
    if (!open) return;
    const loops = await cmd<LoopRegion[]>("loop.list", { song_id: open.song.id });
    openSong.update((o) => (o ? { ...o, loops } : o));
  },

  // --- transport ---

  play: () => cmd("play"),
  pause: () => cmd("pause"),
  seek: (secs: number) => cmd("seek", { secs }),

  async setRate(value: number): Promise<void> {
    const v = Math.min(2.0, Math.max(0.25, value));
    await cmd("rate", { value: v });
    position.update((p) => ({ ...p, rate: v }));
  },

  async setPitch(semitones: number, cents: number): Promise<void> {
    const octaveUp = get(pitch).octaveUp;
    pitch.set({ semitones, cents, octaveUp });
    await cmd("pitch", { semitones, cents, octave_up: octaveUp });
  },

  async setCountIn(
    patch: Partial<{ enabled: boolean; beats: number; loopMode: "first" | "every" }>,
  ): Promise<void> {
    const next = { ...get(countIn), ...patch };
    countIn.set(next);
    await cmd("countin.set", {
      enabled: next.enabled,
      beats: next.beats,
      loop_mode: next.loopMode,
    });
  },

  async setSectionClick(enabled: boolean): Promise<void> {
    sectionClick.set({ enabled });
    await cmd("sectionclick.set", { enabled });
  },

  /** Push the current metronome config to the server (persists all but running). */
  async pushMetronome(): Promise<void> {
    const m = get(metronome);
    await cmd("metronome.set", {
      running: m.running,
      bpm: m.bpm,
      beats_per_bar: m.beatsPerBar,
      strong_mask: strongMask(m.beatsPerBar),
      cadence: m.cadence,
      kit: m.kit,
    });
  },

  /** Patch the metronome config and push. */
  async setMetronome(patch: Partial<MetronomeState>): Promise<void> {
    metronome.update((s) => ({ ...s, ...patch }));
    await this.pushMetronome();
  },

  /** Toggle running. */
  async toggleMetronome(): Promise<void> {
    const running = !get(metronome).running;
    if (!running) metronomeBeat.set(null);
    await this.setMetronome({ running });
  },

  /** Register a tap; when a BPM is derivable, apply it. */
  async tapTempo(now: number): Promise<void> {
    const r = computeTap(tapState, now);
    tapState = r.state;
    if (r.bpm != null) await this.setMetronome({ bpm: r.bpm });
  },

  /** Seed BPM from the open song's analyzed tempo, if any. */
  async syncMetronomeToSong(): Promise<void> {
    const bpm = get(openSong)?.analysis?.bpm;
    if (bpm != null) await this.setMetronome({ bpm: clampBpm(bpm) });
  },

  /** Toggle one section's beat-click guide; server returns refreshed sections. */
  async toggleSectionClick(sectionId: number, on: boolean): Promise<void> {
    const out = await cmd<{ sections: Section[]; orphan_notes: OrphanNote[] }>(
      "section.click.set",
      { section_id: sectionId, on },
    );
    openSong.update((o) =>
      o ? { ...o, sections: out.sections, orphan_notes: out.orphan_notes } : o,
    );
  },

  /** Bass focus: low-pass + the octave-up transcription trick (so the
   *  bassline reads clearly an octave up). Off clears both. */
  async bassFocus(on: boolean): Promise<void> {
    bassFocus.set(on);
    const p = get(pitch);
    pitch.set({ ...p, octaveUp: on });
    await cmd("bass_focus", { on });
    await cmd("pitch", { semitones: p.semitones, cents: p.cents, octave_up: on });
    this.persistIsolation();
  },

  async mute(on: boolean): Promise<void> {
    muted.set(on);
    await cmd("mute", { on });
  },

  /** Live engine volume on every change; the setting write is debounced so
   *  a fader drag lands as one row, not hundreds. */
  async setVolume(value: number): Promise<void> {
    const v = Math.min(1.5, Math.max(0, value));
    playbackVolume.set(v);
    clearTimeout(volumeSaveTimer);
    volumeSaveTimer = setTimeout(() => {
      void this.setSetting(PLAYBACK_VOLUME, v);
    }, 300);
    await cmd("volume", { value: v });
  },

  /** Point the transport at a span without persisting anything (a dumb
   *  primitive used by restart/play; it must NOT touch drill state). */
  async setTransportLoop(start: number, end: number): Promise<void> {
    await cmd("loop.set", { start, end });
  },

  async selectLoop(l: LoopRegion): Promise<void> {
    // set the saved loop, then drop any working loop — they're mutually
    // exclusive and currentLoop is what drives the drill once working is gone.
    currentLoop.set(l);
    workingLoop.set(null);
    await cmd("loop.set", { start: l.start, end: l.end });
  },

  /** The "loop" gesture: spin up a *working* loop over a span — it plays and
   *  drills exactly like a saved loop, but persists nothing until saved. Clears
   *  any selected saved loop and silently replaces a prior working loop. */
  async loopSpan(start: number, end: number): Promise<void> {
    currentLoop.set(null);
    workingLoop.set({ start, end });
    await cmd("loop.set", { start, end });
    await this.seek(start);
    await this.play();
  },

  /** Persist the working loop as a real LoopRegion (adopting an existing loop
   *  with matching bounds rather than duplicating it). Promotes it to the active
   *  saved loop *without* disturbing the live drill: the bounds don't change, so
   *  the bounds-keyed seeder won't reseed and an armed trainer / recall survive.
   *  Set the saved loop FIRST, then clear the working one, so `activeLoop`'s
   *  bounds never momentarily go null between the two writes. */
  async saveWorkingLoop(): Promise<void> {
    const w = get(workingLoop);
    if (!w) return;
    const open = get(openSong);
    const existing = open?.loops.find(
      (l) => Math.abs(l.start - w.start) < 0.01 && Math.abs(l.end - w.end) < 0.01,
    );
    const l = existing ?? (await this.createLoop(w.start, w.end));
    currentLoop.set(l);
    workingLoop.set(null);
  },

  /** Resize the working loop in place (right-drag on the waveform). Moves its
   *  bounds and follows the drill home/scratch to them WITHOUT a teardown — the
   *  trainer and any opened tools survive, mirroring a saved-loop resize. Save
   *  persists these bounds. Pre-setting `lastDrillBounds` makes the seeder treat
   *  this as the same engagement rather than a fresh loop. */
  async setWorkingLoopBounds(start: number, end: number): Promise<void> {
    if (!get(workingLoop)) return;
    lastDrillBounds = `${start},${end}`;
    drillHome.set({ start, end });
    drillSpan.set({ start, end });
    workingLoop.set({ start, end });
    await cmd("loop.set", { start, end });
  },

  async clearTransportLoop(): Promise<void> {
    workingLoop.set(null);
    currentLoop.set(null);
    await cmd("loop.clear");
  },

  // --- drill box: live edits to the scratch span (saved loops untouched) ---

  /** (Re)seed the drill from an engaged loop's bounds, or tear it down (null).
   *  Sets both the home and the live scratch span, and resets live drill state
   *  (disarm trainer, zero cycle, clear recall) so nothing leaks across loops. */
  seedDrill(span: Span | null): void {
    drillHome.set(span);
    drillSpan.set(span);
    drillTrainer.update((t) => ({ ...t, armed: false, cycle: 0 }));
    drillShow.set({ trainer: false, recall: false, region: false });
    void this.clearRecall();
  },

  /** Reveal a drill tool (opt-in; doesn't change playback by itself). */
  showDrillTool(tool: "trainer" | "recall" | "region"): void {
    drillShow.update((s) => ({ ...s, [tool]: true }));
  },

  /** Hide a drill tool and undo its effect, so the loop plays normally again. */
  async hideDrillTool(tool: "trainer" | "recall" | "region"): Promise<void> {
    drillShow.update((s) => ({ ...s, [tool]: false }));
    if (tool === "trainer") {
      this.disarmTrainer();
      await this.resetRate();
    } else if (tool === "recall") {
      await this.clearRecall();
    } else {
      await this.drillResetSpan();
    }
  },

  /** Set the scratch span and point the transport at it. */
  async applyDrillSpan(span: Span): Promise<void> {
    drillSpan.set(span);
    await cmd("loop.set", { start: span.start, end: span.end });
  },

  /** Grid lines for the current subdivision, and the downbeats, for snapping. */
  drillGrid(): { grid: number[]; downbeats: number[] } {
    const a = get(openSong)?.analysis;
    if (!a) return { grid: [], downbeats: [] };
    return { grid: subdivisionTimes(a.beats, a.downbeats, get(gridSubdivision)), downbeats: a.downbeats };
  },

  drillDuration(): number {
    return get(openSong)?.song.duration_secs ?? get(drillSpan)?.end ?? 0;
  },

  /** Move one scratch edge by a grid step (or 0.25 s without a grid). */
  async drillNudge(edge: "start" | "end", dir: 1 | -1): Promise<void> {
    const span = get(drillSpan);
    if (!span) return;
    await this.applyDrillSpan(nudgeEdge(span, edge, dir, this.drillGrid().grid, this.drillDuration(), 0.25));
  },

  /** Shrink the scratch span to its first or second half. */
  async drillIsolate(half: "first" | "second"): Promise<void> {
    const span = get(drillSpan);
    if (!span) return;
    await this.applyDrillSpan(bisect(span, half, this.drillGrid().grid));
  },

  /** Extend (+) or retract (−) the scratch start by N bars to drill the entrance. */
  async drillRunUp(deltaBars: number): Promise<void> {
    const span = get(drillSpan);
    if (!span) return;
    await this.applyDrillSpan(runUp(span, deltaBars, this.drillGrid().downbeats, this.drillDuration()));
  },

  /** Snap the scratch span back to the drill's home bounds. */
  async drillResetSpan(): Promise<void> {
    const home = get(drillHome);
    if (home) await this.applyDrillSpan({ ...home });
  },

  // --- drill box: step-up tempo trainer ---

  /** Arm the trainer: reset the cycle and apply the recipe's rep-0 rate now;
   *  each subsequent loop wrap advances it (see the `loop_wrapped` handler). */
  async armTrainer(): Promise<void> {
    drillTrainer.update((t) => ({ ...t, armed: true, cycle: 0 }));
    await this.setRate(rateForRep(get(drillTrainer).recipe, 0));
  },

  disarmTrainer(): void {
    drillTrainer.update((t) => ({ ...t, armed: false }));
  },

  /** Toggle the trainer — the drill box's primary verb (the `d` key). */
  async toggleTrainer(): Promise<void> {
    if (get(drillTrainer).armed) this.disarmTrainer();
    else await this.armTrainer();
  },

  /** Edit the ramp recipe; re-apply at the current cycle if already armed. */
  async setTrainerRecipe(recipe: TempoCurve): Promise<void> {
    drillTrainer.update((t) => ({ ...t, recipe }));
    const t = get(drillTrainer);
    if (t.armed) await this.setRate(rateForRep(recipe, t.cycle));
  },

  /** Return the global rate to 100% (the trainer leaves it where it landed). */
  async resetRate(): Promise<void> {
    await this.setRate(1.0);
  },

  // --- drill box: mute-to-recall ---

  /** Silence the next loop pass (play it from memory). */
  armRecallNext(): void {
    drillRecall.update((r) => ({ ...r, armNext: true }));
  },

  /** Silence every Nth pass (null disables). Resets the pass counter. */
  async setRecallEveryN(n: number | null): Promise<void> {
    drillPass = 0;
    drillRecall.update((r) => ({ ...r, everyN: n }));
    if (n === null) await this.maybeUnmuteRecall();
  },

  /** Fully clear recall and hand the mute back (used on teardown). */
  async clearRecall(): Promise<void> {
    drillRecall.set({ everyN: null, armNext: false });
    drillPass = 0;
    await this.maybeUnmuteRecall();
  },

  /** Restore audio iff recall (not the user) was holding the mute. */
  async maybeUnmuteRecall(): Promise<void> {
    if (recallMuted) {
      recallMuted = false;
      await this.mute(false);
    }
  },

  /** Reset the stage to a clean slate: stop playback, refit the waveform zoom,
   *  drop the selection, the clicked active span, the active loop, return the
   *  playhead to the start, and restore speed to 100% and pitch to 0 — without
   *  touching volume. The zoom + active span live as local state in Waveform,
   *  so we signal it via workspaceReset. */
  async resetWorkspace(): Promise<void> {
    selection.set(null);
    if (get(position).playing) await this.pause();
    await this.clearTransportLoop();
    await this.seek(0);
    await this.setRate(1.0);
    await this.setPitch(0, 0);
    workspaceReset.update((n) => n + 1);
  },

  // --- annotations ---

  /** Persist a loop for the current span; the server names it dynamically. */
  async createLoop(start: number, end: number): Promise<LoopRegion> {
    const open = get(openSong);
    if (!open) throw new Error("no song open");
    const l = await cmd<LoopRegion>("loop.create", {
      song_id: open.song.id,
      start,
      end,
    });
    await this.refreshLoops();
    return l;
  },

  /** Snap a loop's edges to the nearest section boundaries (renames it). */
  async fitLoop(loopId: number): Promise<void> {
    await cmd("loop.fit", { loop_id: loopId });
    await this.refreshLoops();
  },

  async updateLoop(
    loopId: number,
    fields: { name?: string; start?: number; end?: number },
  ): Promise<void> {
    await cmd("loop.update", { loop_id: loopId, ...fields });
    await this.refreshLoops();
  },

  async deleteLoop(loopId: number): Promise<void> {
    await cmd("loop.delete", { loop_id: loopId });
    if (get(currentLoop)?.id === loopId) await this.clearTransportLoop();
    await this.refreshLoops();
  },

  /** Replace the whole section lane. */
  async replaceSections(
    sections: {
      name: string;
      start: number;
      end: number;
      position: number;
      clickGuide?: boolean;
    }[],
  ): Promise<void> {
    const open = get(openSong);
    if (!open) return;
    const out = await cmd<{ sections: Section[]; orphan_notes: OrphanNote[] }>("section.replace", {
      song_id: open.song.id,
      sections: sections.map((s) => ({
        name: s.name,
        start: s.start,
        end: s.end,
        position: s.position,
        click_guide: s.clickGuide ?? false,
      })),
    });
    openSong.update((o) =>
      o ? { ...o, sections: out.sections, orphan_notes: out.orphan_notes } : o,
    );
    await this.refreshLoops();
  },

  /** Save a section's notes by occurrence label; empty doc clears it. The
   *  server returns the refreshed sections + orphan list, which we mirror. */
  async setSectionNotes(label: string, doc: NotesDoc): Promise<void> {
    if (!get(openSong)) return;
    const out = await cmd<{ sections: Section[]; orphan_notes: OrphanNote[] }>(
      "section.notes.set",
      { label, doc },
    );
    openSong.update((o) =>
      o ? { ...o, sections: out.sections, orphan_notes: out.orphan_notes } : o,
    );
  },

  // --- output devices ---

  async refreshOutputs(): Promise<void> {
    outputDevices.set(await cmd<AudioDevice[]>("device.outputs"));
  },

  async setOutputDevice(id: string | null): Promise<void> {
    outputDevice.set(id);
    // device.setOutput owns persistence (writes the output_device setting),
    // so there is no separate setSetting here.
    await cmd("device.setOutput", { id });
  },

  async refreshInputs(): Promise<void> {
    inputDevices.set(await cmd<AudioDevice[]>("device.inputs"));
  },

  async setInputDevice(id: string | null): Promise<void> {
    inputDevice.set(id);
    // device.setInput owns persistence (writes the input_device setting).
    await cmd("device.setInput", { id });
  },

  // --- tuner ---
  //
  // The tuner shares the devices tab's input list (`inputDevices` / `device.inputs`)
  // — it keeps no list of its own. Its target is resolved through the chain
  // tuner override → global input → system default → first input.

  /** Power on: resolve the effective input id and start capture. */
  async tunerPowerOn(): Promise<void> {
    await this.refreshInputs();
    const id = resolveInputDevice(get(tunerInput), get(inputDevice), get(inputDevices));
    if (id === null) throw new Error("no audio input devices found");
    await cmd("tuner.start", { device_id: id });
    tunerOn.set(true);
  },

  async tunerPowerOff(): Promise<void> {
    await cmd("tuner.stop");
    tunerOn.set(false);
    tunerReading.set(null);
  },

  /** Pick a tuner input (an `AudioDevice.id` or `"default"`); persist it and
   *  restart capture on the resolved id if already on. */
  async setTunerInput(sel: string): Promise<void> {
    tunerInput.set(sel);
    await this.setSetting(TUNER_INPUT, sel);
    if (get(tunerOn)) {
      const id = resolveInputDevice(sel, get(inputDevice), get(inputDevices));
      if (id === null) throw new Error("no audio input devices found");
      await cmd("tuner.start", { device_id: id });
    }
  },

  // --- stems ---

  /** The stems-length gain vector sent to the engine: sliders × mute × solo.
   *  Mirrored in Rust by `Isolation::resolve_gains` (crates/practice/src/model.rs)
   *  — keep the fold logic in lockstep. */
  stemGainsVector(mix: StemMix): number[] {
    const anySolo = mix.solos.some(Boolean);
    return mix.levels.map((level, i) =>
      mix.mutes[i] || (anySolo && !mix.solos[i]) ? 0 : level / 100,
    );
  },

  async applyStemMix(): Promise<void> {
    if (!get(openSong)?.stems) return;
    await cmd("stems.gains", { gains: this.stemGainsVector(get(stemMix)) });
  },

  /** Debounced save of the live isolation state (bass focus + stem mix) to the
   *  open song's manifest. A fader drag thus writes once, when it settles —
   *  not once per tick. The `song_id` is captured now so a save landing after
   *  a song switch still writes the song it was edited on. */
  persistIsolation(): void {
    activeSnapshotSlot.set(null);
    const open = get(openSong);
    if (!open) return;
    const song_id = open.song.id;
    const iso = stemMixToIsolation(get(stemMix), get(bassFocus));
    clearTimeout(isolationSaveTimer);
    isolationSaveTimer = setTimeout(() => {
      void cmd("isolation.set", { song_id, ...iso });
    }, 350);
  },

  async setStemLevel(idx: number, level: number): Promise<void> {
    stemMix.update((m) => ({ ...m, levels: m.levels.map((v, i) => (i === idx ? level : v)) }));
    await this.applyStemMix();
    this.persistIsolation();
  },

  async toggleStemMute(idx: number): Promise<void> {
    stemMix.update((m) => ({ ...m, mutes: m.mutes.map((v, i) => (i === idx ? !v : v)) }));
    await this.applyStemMix();
    this.persistIsolation();
  },

  async toggleStemSolo(idx: number): Promise<void> {
    stemMix.update((m) => ({ ...m, solos: m.solos.map((v, i) => (i === idx ? !v : v)) }));
    await this.applyStemMix();
    this.persistIsolation();
  },

  /** Restore all faders to 100% and clear every mute/solo, in one engine call. */
  async resetStemMix(): Promise<void> {
    stemMix.set(defaultStemMix());
    await this.applyStemMix();
    this.persistIsolation();
  },

  // --- routines ---

  /** Snapshot the live isolation state as a `Mix` (the block's "what you hear").
   *  Resolved gains — mute/solo folded in — matching the backend contract. */
  captureMix(): Mix {
    return { bass_focus: get(bassFocus), stems: this.stemGainsVector(get(stemMix)) };
  },

  /** Build a block from the current span (active loop → drill span → whole song),
   *  speed, and mix. The author tweaks fields afterward. */
  captureBlock(): Block {
    const open = get(openSong);
    const loop = get(currentLoop);
    const drill = get(drillSpan);
    const span = loop
      ? { start: loop.start, end: loop.end }
      : drill
        ? { start: drill.start, end: drill.end }
        : { start: 0, end: open?.song.duration_secs ?? 0 };
    return {
      span,
      mix: this.captureMix(),
      speed: get(position).rate,
      passes: 1,
      lead_in_beats: 0,
      count_in: { beats: 0, loop_mode: "first" },
      name: null,
    };
  },

  // --- isolation snapshots ---

  /** The live isolation-box state in wire shape (what snapshot.save stores). */
  captureIsolation(): Isolation {
    return stemMixToIsolation(get(stemMix), get(bassFocus));
  },
  saveSnapshot: (slot: number) =>
    cmd("isolation.snapshot.save", { slot, state: actions.captureIsolation() }),
  activateSnapshot: (slot: number) => cmd("isolation.snapshot.activate", { slot }),
  clearSnapshot: (slot: number) => cmd("isolation.snapshot.clear", { slot }),
  cycleSnapshots: () => cmd("isolation.snapshot.cycle"),

  // --- markers / pedal ---

  setMarker: (slot: number) => cmd("marker.set", { slot }), // pos defaults to playhead
  clearMarker: (slot: number) => cmd("marker.clear", { slot }),
  playMarker: (slot: number) => cmd("marker.play", { slot }),
  setPedalMapping(rows: PedalBinding[]): Promise<void> {
    return this.setSetting(PEDAL_MAPPING, rows);
  },

  async refreshRoutines(): Promise<void> {
    const open = get(openSong);
    if (!open) return;
    const routines = await cmd<Routine[]>("routine.list", { song_id: open.song.id });
    openSong.update((o) => (o ? { ...o, routines } : o));
  },

  /** Upsert a routine (id 0 = new) and refresh the list; returns the stored
   *  routine (with its real id), or null when no song is open. */
  async saveRoutine(routine: Routine): Promise<Routine | null> {
    const open = get(openSong);
    if (!open) return null;
    const saved = await cmd<Routine>("routine.save", { song_id: open.song.id, routine });
    await this.refreshRoutines();
    return saved;
  },

  async deleteRoutine(id: number): Promise<void> {
    const open = get(openSong);
    if (!open) return;
    await cmd("routine.delete", { song_id: open.song.id, id });
    if (get(activeRoutine)?.routine_id === id) activeRoutine.set(null);
    await this.refreshRoutines();
  },

  /** Launch a routine, optionally jumping straight into `blockIndex`. */
  async startRoutine(id: number, blockIndex = 0): Promise<void> {
    const open = get(openSong);
    if (!open) return;
    const status = await cmd<RoutineStatus>("routine.start", {
      song_id: open.song.id,
      id,
      block_index: blockIndex,
    });
    activeRoutine.set(status);
    applyRoutineMix(status.block.mix);
  },

  async stopRoutine(): Promise<void> {
    await cmd("routine.stop");
    activeRoutine.set(null);
  },

  // --- recordings ---

  /** Trigger an armed take from the transport: resolve the configured span +
   *  input and start recording. Range logic lives here so the transport button
   *  stays dumb (the recordings box only arms). */
  async triggerRecord(): Promise<void> {
    await this.refreshInputs();
    const span = get(recordSpan);
    const resolved = resolveInputDevice(get(recordInput), get(inputDevice), get(inputDevices));
    if (!resolved) {
      traceErr("record", "no input device");
      return;
    }
    const backendSpan: "song" | "selection" | "loop" = span === "playhead" ? "selection" : span;
    const sel = get(selection);
    const lp = get(currentLoop);
    const pos = get(position);
    const open = get(openSong);
    const range =
      span === "playhead" ? { start: pos.secs, end: open?.song.duration_secs ?? 0 }
      : span === "selection" && sel ? { start: sel.start, end: sel.end }
      : span === "loop" && lp ? { start: lp.start, end: lp.end }
      : undefined;
    await this.startRecording(backendSpan, resolved, range);
  },

  async startRecording(span: "song" | "selection" | "loop", deviceId: string, range?: { start: number; end: number }): Promise<void> {
    // optimistic: flip active before the round-trip so the record button can't
    // double-fire; roll back if the command is rejected (e.g. no device).
    recordingActive.set(true);
    try {
      await cmd("recording.start", { span, device_id: deviceId, ...(range ?? {}) });
    } catch (e) {
      recordingActive.set(false);
      throw e;
    }
  },

  async stopRecording(): Promise<void> {
    const rec = await cmd<Recording>("recording.stop");
    recordingActive.set(false);
    recordings.update((rs) => (rs.some((r) => r.id === rec.id) ? rs : [...rs, rec]));
  },

  /** Start the input-level monitor on `deviceId` (the arming meter). Failures
   *  are non-fatal — the meter just stays blank. */
  async startInputMonitor(deviceId: string): Promise<void> {
    try {
      await cmd("input.monitorStart", { device_id: deviceId });
    } catch (e) {
      traceErr("recordings", `input.monitorStart failed: ${e}`);
    }
  },

  async stopInputMonitor(): Promise<void> {
    inputLevel.set(null);
    try {
      await cmd("input.monitorStop");
    } catch (e) {
      traceErr("recordings", `input.monitorStop failed: ${e}`);
    }
  },

  async deleteRecording(id: number): Promise<void> {
    await cmd("recording.delete", { id });
    recordings.update((rs) => rs.filter((r) => r.id !== id));
  },

  async renameRecording(id: number, name: string): Promise<void> {
    await cmd("recording.rename", { id, name });
    recordings.update((rs) => rs.map((r) => (r.id === id ? { ...r, name } : r)));
  },

  async setRecordingGain(id: number, gain: number): Promise<void> {
    recordings.update((rs) => rs.map((r) => (r.id === id ? { ...r, gain } : r)));
    await cmd("recording.setGain", { id, gain });
  },

  async toggleRecordingMute(id: number): Promise<void> {
    let muted = false;
    recordings.update((rs) => rs.map((r) => (r.id === id ? ((muted = !r.muted), { ...r, muted }) : r)));
    await cmd("recording.setMute", { id, muted });
  },

  async setRecordingNudge(id: number, nudgeFrames: number): Promise<void> {
    recordings.update((rs) => rs.map((r) => (r.id === id ? { ...r, nudge_frames: nudgeFrames } : r)));
    await cmd("recording.setNudge", { id, nudge_ms: framesToMs(nudgeFrames) });
  },

  /** Pull the latency status (both measurements + active source) into the store. */
  async refreshLatency(): Promise<void> {
    latencyStatus.set(await cmd<LatencyStatus>("recording.latency"));
  },

  async calibrateLatency(deviceId: string): Promise<CalibrationResult> {
    const result = await cmd<CalibrationResult>("recording.calibrate", { device_id: deviceId });
    await this.refreshLatency();
    return result;
  },

  async resetLatency(): Promise<void> {
    await cmd("recording.calibrate.reset");
    await this.refreshLatency();
  },

  // --- prepare (analysis → stems) ---

  /** One button: structure/beat analysis, then stem separation —
   *  sequentially, never in parallel (both are GPU-heavy; SongFormer alone
   *  peaks ~8 GiB of VRAM). The modal mirrors prepareState; each step is
   *  resolved by its `*_progress` event or a `cached` short-circuit, and a
   *  failure never blocks the other step. */
  async prepare({ forceAnalysis = false, forceStems = false } = {}): Promise<void> {
    const open = get(openSong);
    if (!open || get(prepareState)) return;
    const id = open.song.id;
    prepareSongId = id;
    prepareCancelled = false;
    prepareState.set({
      open: true,
      song_id: id,
      steps: { analysis: "pending", stems: "pending" },
      errors: {},
    });
    workSample.set(null);
    vram.set(null);

    const run = async (
      step: "analysis" | "stems",
      command: string,
      extra: Record<string, unknown> = {},
    ): Promise<void> => {
      setPrepareStep(step, "running");
      try {
        // register before dispatch — the terminal event must not slip past
        const report = new Promise<{ state: string; error?: string }>((resolve) => {
          prepareWaiters[step] = resolve;
        });
        const out = await cmd<{ state: string }>(command, { song_id: id, ...extra });
        if (out.state === "cached") {
          delete prepareWaiters[step];
          setPrepareStep(step, "cached");
          return;
        }
        const r = await report;
        if (r.state === "done") setPrepareStep(step, "done");
        else if (r.state === "cancelled") setPrepareStep(step, "cancelled");
        else setPrepareStep(step, "failed", r.error ?? `${step} failed`);
      } catch (e) {
        // shown verbatim: install/setup hints ride on the error message
        delete prepareWaiters[step];
        setPrepareStep(step, "failed", e instanceof Error ? e.message : String(e));
      }
    };

    // Both steps are cached by default; each force flag invalidates its own
    // cache (fresh SongFormer sections / re-separated stems) independently. A
    // stop during analysis skips the stems step entirely.
    await run("analysis", "analysis.run", forceAnalysis ? { force: true } : {});
    if (!prepareCancelled) {
      await run("stems", "stems.separate", forceStems ? { force: true } : {});
    }
    prepareSongId = null;

    // refresh exactly as the scattered flows did: re-open auto-loads cached
    // stems + analysis, loadAnalysis surfaces the section suggestions. A step
    // that finished before the stop keeps its result, so this still runs.
    if (get(openSong)?.song.id === id) {
      await this.openSong(id);
      const s = get(prepareState);
      if (s && (s.steps.analysis === "done" || s.steps.analysis === "cached")) {
        await this.loadAnalysis(id);
      }
    }
    // a stop closes the modal straight away — nothing to linger over
    if (prepareCancelled) {
      this.closePrepare();
      return;
    }
    const s = get(prepareState);
    const ok = (st: PrepareStepState) => st === "done" || st === "cached";
    if (s && ok(s.steps.analysis) && ok(s.steps.stems)) {
      // all green: linger just long enough to read the two ✓s
      setTimeout(() => { prepareState.set(null); workSample.set(null); vram.set(null); }, 1500);
    }
    // failures leave the modal open with its close button
  },

  /** Stop the in-flight prepare: skip any later step and kill the running
   *  subprocess server-side. Completed steps keep their results. */
  cancelPrepare(): void {
    if (prepareSongId == null) return;
    prepareCancelled = true;
    void cmd("prepare.cancel", { song_id: prepareSongId });
  },

  closePrepare(): void {
    prepareState.set(null);
    workSample.set(null);
    vram.set(null);
  },

  // --- analysis ---

  /** Pull the cached analysis into the open song and surface suggestions. */
  async loadAnalysis(songId: number): Promise<void> {
    const analysis = await cmd<Analysis | null>("analysis.get", { song_id: songId });
    if (get(openSong)?.song.id !== songId) return;
    openSong.update((o) => (o ? { ...o, analysis } : o));
    // Default the count-in beat count to the song's meter (time-signature
    // numerator) when analysis lands — the user can still override it. Only
    // runs at analysis time, so a manual override survives reopening the song.
    const per = meterNumerator(analysis);
    if (per !== null && get(countIn).beats !== per) {
      void actions.setCountIn({ beats: per });
    }
  },
};

// --- drill box lifecycle ----------------------------------------------------
// Whenever the ACTIVE loop's bounds change — a different loop engaged (working
// or saved), or all loops cleared (incl. via resetWorkspace / song open) —
// reseed the scratch span and tear down live drill state so nothing leaks
// across loops: disarm the trainer, zero its cycle, and clear recall (which
// hands the engine mute back if recall held it).
//
// Keyed on bounds, NOT identity: promoting a working loop to a saved one keeps
// the same bounds (it only gains an id + name), so the seeder skips it and an
// armed trainer / recall survive the save. `lastDrillBounds` is hoisted up top
// so `setWorkingLoopBounds` can pre-seed it for the same reason on resize.
activeLoop.subscribe((al) => {
  const key = al ? `${al.start},${al.end}` : null;
  if (key === lastDrillBounds) return;
  lastDrillBounds = key;
  actions.seedDrill(al ? { start: al.start, end: al.end } : null);
});

// --- launch restore ---------------------------------------------------------

const LAST_SONG_KEY = "dredge-last-song";

/** Pick up where the last session left off (`DREDGE_OPEN` wins when set);
 *  the song may be gone — start empty rather than surfacing an error. */
async function openLastSong(): Promise<void> {
  const forced = await initialSong().catch(() => null);
  const stored = Number(localStorage.getItem(LAST_SONG_KEY));
  const id = forced ?? (Number.isInteger(stored) && stored > 0 ? stored : null);
  if (id == null) return;
  try {
    await actions.openSong(id);
  } catch {
    // song may be gone — start empty
  }
}

// --- events ----------------------------------------------------------------

/** Animate the isolation faders to a routine block's mix (display only — the
 *  backend already applied it to the engine, with its own gain slew). Glides
 *  the levels over a short window so the faders don't lurch between blocks. */
let routineMixRaf = 0;
function applyRoutineMix(mix: Mix): void {
  bassFocus.set(mix.bass_focus);
  const target = mix.stems.map((g) => Math.max(0, Math.min(100, g * 100)));
  const start = get(stemMix).levels.slice();
  const t0 = performance.now();
  const DUR = 220;
  cancelAnimationFrame(routineMixRaf);
  const tick = (now: number) => {
    const k = Math.min(1, (now - t0) / DUR);
    const levels = target.map((tl, i) => (start[i] ?? tl) + (tl - (start[i] ?? tl)) * k);
    // Routine mix is absolute gains — mute/solo are folded in, so clear them.
    const off = Array(STEM_LABELS.length).fill(false);
    stemMix.set({ levels, mutes: off, solos: [...off] });
    if (k < 1) routineMixRaf = requestAnimationFrame(tick);
  };
  routineMixRaf = requestAnimationFrame(tick);
}

/** Sequence counter for `lastMidiTrigger` — forces reactivity when the same
 *  trigger fires twice in a row. */
let midiSeq = 0;

export async function initEvents(): Promise<() => void> {
  void openLastSong();
  return onEvent((ev) => {
    switch (ev.event) {
      case "position": {
        const d = ev.data as {
          secs: number;
          rate: number;
          playing: boolean;
          count_in: { beat: number; of: number } | null;
        };
        position.set({
          secs: d.secs,
          rate: d.rate,
          playing: d.playing,
          at: performance.now(),
          countIn: d.count_in ?? null,
        });
        break;
      }
      case "metronome_beat": {
        const d = ev.data as { beat: number; of: number; sounded: boolean };
        metronomeBeat.set(d);
        break;
      }
      case "loop_wrapped": {
        // drill trainer: advance the cycle and let the global rate follow the
        // recipe.
        const t = get(drillTrainer);
        if (t.armed) {
          const cycle = t.cycle + 1;
          drillTrainer.set({ ...t, cycle });
          void actions.setRate(rateForRep(t.recipe, cycle));
        }
        // recall: decide whether the pass that just began plays from memory.
        const r = get(drillRecall);
        if (r.everyN != null || r.armNext) {
          drillPass += 1;
          const silent = r.armNext || (r.everyN != null && drillPass % r.everyN === 0);
          if (silent !== recallMuted) {
            recallMuted = silent;
            void actions.mute(silent);
          }
          if (r.armNext) drillRecall.set({ ...r, armNext: false });
        }
        break;
      }
      case "routine": {
        const status = ev.data as RoutineStatus;
        activeRoutine.set(status);
        applyRoutineMix(status.block.mix);
        break;
      }
      case "stems_progress": {
        const data = ev.data as { song_id: number; state: string; error?: string };
        const waiter = takePrepareWaiter("stems", data);
        if (waiter) {
          // prepare() owns the modal update and the end-of-flow refresh
          waiter(data);
          break;
        }
        if (data.state === "done") {
          // re-opening the song auto-loads the freshly cached stems
          if (get(openSong)?.song.id === data.song_id) void actions.openSong(data.song_id);
        } else if (data.state === "failed") {
          stemsError.set(data.error ?? "stem separation failed");
        }
        break;
      }
      case "analysis_progress": {
        const data = ev.data as {
          song_id: number;
          state: string;
          error?: string;
          sections?: Section[];
        };
        // analysis now auto-commits its sections server-side: apply the saved
        // layout and refresh loops (section changes may have pruned some) so the
        // structure is live without a manual save. Runs for the prepare flow too.
        if (data.state === "done" && get(openSong)?.song.id === data.song_id) {
          if (data.sections) {
            openSong.update((o) => (o ? { ...o, sections: data.sections! } : o));
          }
          void actions.loadAnalysis(data.song_id);
          void actions.refreshLoops();
        } else if (data.state === "failed") {
          analysisError.set(data.error ?? "analysis failed");
        }
        const waiter = takePrepareWaiter("analysis", data);
        if (waiter) waiter(data);
        break;
      }
      case "work_sample":
        actions.recordWorkSample(ev.data as WorkSample);
        break;
      case "tuner_pitch":
        tunerReading.set(ev.data as TunerReading);
        break;
      case "input_level":
        inputLevel.set(ev.data as { peak: number; rms: number });
        break;
      case "profile_run":
        actions.recordProfile(ev.data as ProfileRun);
        break;
      case "library_changed":
        // socket-driven imports land in the sidebar
        void actions.refreshSongs();
        break;
      case "recording.finished": {
        const r = ev.data as Recording;
        recordings.update((rs) => (rs.some((x) => x.id === r.id) ? rs : [...rs, r]));
        recordingActive.set(false);
        break;
      }
      case "markers": {
        const d = ev.data as { song_id: number; markers: Marker[] };
        openSong.update((o) => (o && o.song.id === d.song_id ? { ...o, markers: d.markers } : o));
        break;
      }
      case "snapshots": {
        const d = ev.data as { song_id: number; snapshots: IsolationSnapshot[] };
        openSong.update((o) =>
          o && o.song.id === d.song_id ? { ...o, snapshots: d.snapshots } : o,
        );
        break;
      }
      case "isolation": {
        // A snapshot was applied server-side: the engine already has the mix,
        // so only mirror it into the UI stores — never re-send stems.gains or
        // bass_focus. Keeps the bass-focus/pitch-octave coupling in step with
        // the restore path in actions.openSong / actions.bassFocus.
        const d = ev.data as { song_id: number; isolation: Isolation; slot: number | null };
        const open = get(openSong);
        if (!open || open.song.id !== d.song_id) break;
        // Cancel any pending debounced persist — it captured pre-snapshot state
        // and would otherwise clobber what the server just applied and saved.
        clearTimeout(isolationSaveTimer);
        stemMix.set(isolationToStemMix(d.isolation));
        bassFocus.set(d.isolation.bass_focus);
        activeSnapshotSlot.set(d.slot);
        const p = get(pitch);
        if (p.octaveUp !== d.isolation.bass_focus) {
          pitch.set({ ...p, octaveUp: d.isolation.bass_focus });
          void cmd("pitch", {
            semitones: p.semitones,
            cents: p.cents,
            octave_up: d.isolation.bass_focus,
          });
        }
        break;
      }
      case "midi": {
        midiSeq += 1;
        lastMidiTrigger.set({ trigger: (ev.data as { trigger: string }).trigger, seq: midiSeq });
        break;
      }
    }
  });
}
