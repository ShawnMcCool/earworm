<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { fade } from "svelte/transition";
  import { get } from "svelte/store";
  import { canvasSize } from "../lib/actions/canvasSize";
  import {
    actions,
    activePlay,
    activeRoutine,
    allLoopsVisible,
    clickSnap,
    currentLoop,
    drillSpan,
    gridLines,
    gridSnap,
    gridSubdivision,
    gridVisible,
    openingSong,
    openSong,
    position,
    recordings,
    selection,
    workingLoop,
    workspaceReset,
    type LoopRegion,
    type OpenSong,
  } from "../lib/stores";
  import HoverActions from "../lib/ui/HoverActions.svelte";
  import {
    hitLaneSpan as hitLane,
    hitLoopBody as hitBody,
    hitLoopEdge as hitEdge,
    hitMarkerPip as hitPip,
    laneSpans,
    nearestLoopEdge as nearestEdge,
    spanAtTime as spanAt,
    MARKER_PIP_H,
    MARKER_PIP_W,
    type LaneSpan,
    type LoopEdge,
  } from "../lib/waveform-hit";
  import { THEME_EVENT } from "../lib/theme";
  import { hexToHue, labelColor } from "../lib/waveform-colors";
  import {
    adjustWindow,
    clampToLoop,
    followView,
    makePlayheadClock,
    secToX,
    snapToGrid,
    subdivisionTimes,
    tickPlayhead,
    visibleBuckets,
    xToSec,
    zoom,
    type View,
  } from "../lib/waveform-math";
  import { layerSpanSecs } from "../lib/recording-math";

  const GRID_SUBDIVS = ["bar", "beat", "eighth"] as const;
  const SAMPLE_RATE = 48000;
  const LANE_H = 24; // section lane above the waveform
  const WAVE_H = 200;
  const LAYER_LANE_H = 30; // height of each recording layer lane below the waveform
  const EDGE_PX = 4; // loop-edge hit zone
  const CLICK_PX = 5; // below this a drag is a click → seek
  const SNAP_PX = 10; // grid-snap pull radius around a downbeat
  const MIN_TICK_PX = 6; // hide beat ticks when they'd sit closer than this
  const DBLCLICK_MS = 300; // two lane clicks within this = double-click

  let canvas: HTMLCanvasElement;
  let view: View = $state({ startSec: 0, endSec: 1, width: 1 });
  let lastSongId: number | null = null;
  /** Lane span whose bounds currently drive the transport loop (clicked). */
  let activeSpan: { start: number; end: number } | null = $state(null);

  // "lock viewport to playhead": while playing, scroll the window so the
  // playhead stays within the centre dead-zone band. Ephemeral local state —
  // off by default, reset on new song / workspace reset. A manual pan turns it
  // off (the pointer handlers below); zoom stays follow-aware.
  const FOLLOW_MARGIN = 0.2; // free-roam middle 60%, scroll past the edge 20%
  let follow = $state(false);
  // grid/snap toolbar: collapsed to a corner icon by default, clicks expand it
  let gridOpen = $state(false);
  // last playhead drawn — used as the zoom anchor while following so a zoom
  // can't shove the playhead out of view (recomputing the smoothed clock here
  // would perturb its interpolation, so reuse what draw() last produced).
  let lastPlayhead = 0;

  // pointer interaction state
  type Drag =
    | { mode: "select"; anchorX: number; moved: boolean }
    | { mode: "resize"; loopId: number | null; edge: "start" | "end"; start: number; end: number; start0: number; end0: number }
    | { mode: "lane"; anchor: { start: number; end: number }; moved: boolean; double: boolean }
    | { mode: "zoom"; anchorX: number; curX: number };
  let drag: Drag | null = null;
  // timestamp of the last lane-header pointerdown — for double-click detection
  let lastLaneDownAt = 0;
  // after a resize is released, hold the new bounds visually until the
  // loop.update round-trip lands in the store — otherwise the wave flashes back
  // to the old bounds for the frames between release and refresh.
  let pendingResize: { id: number; start: number; end: number } | null = null;

  /** Zoom out to frame the whole song (keeps the current canvas width). */
  function fitToSong(open: OpenSong) {
    view = { startSec: 0, endSec: Math.max(open.song.duration_secs, 2), width: view.width };
  }

  // reset the view when a different song opens
  $effect(() => {
    const open = $openSong;
    if (open && open.song.id !== lastSongId) {
      lastSongId = open.song.id;
      activeSpan = null;
      follow = false;
      fitToSong(open);
    }
  });

  // workspace reset (controls box) — refit zoom + drop the clicked active span;
  // the selection/loop/playhead are cleared store-side by resetWorkspace().
  let lastReset = 0;
  $effect(() => {
    const n = $workspaceReset;
    if (n !== lastReset) {
      lastReset = n;
      activeSpan = null;
      follow = false;
      const open = get(openSong);
      if (open) fitToSong(open);
    }
  });

  function duration(): number {
    return get(openSong)?.song.duration_secs ?? 0;
  }

  // laneSpans is a pure function of the open song; memoize on the song object's
  // identity so the per-redraw draw and the per-pointer-move hit-tests don't
  // rebuild the array every call.
  let laneSpansCache: { open: OpenSong; spans: LaneSpan[] } | null = null;
  function spansFor(open: OpenSong): LaneSpan[] {
    if (laneSpansCache?.open !== open) {
      laneSpansCache = { open, spans: laneSpans(open) };
    }
    return laneSpansCache.spans;
  }

  function loopBounds(l: LoopRegion): { start: number; end: number } {
    if (drag?.mode === "resize" && drag.loopId === l.id) {
      return { start: drag.start, end: drag.end };
    }
    if (pendingResize?.id === l.id) {
      return { start: pendingResize.start, end: pendingResize.end };
    }
    return { start: l.start, end: l.end };
  }

  // Demand-driven redraw: instead of an unconditional 60fps RAF (which repaints
  // an identical canvas while paused/idle), schedule a single frame on demand
  // and keep animating ONLY while playing (so the interpolated playhead stays
  // smooth). Everything draw() reads reactively is wired to requestRedraw via
  // the $effect below; the non-reactive `drag`/`pendingResize` are nudged from
  // the pointer handlers.
  let rafPending = false;
  let rafId = 0;
  const playClock = makePlayheadClock();

  // count-in playhead pulse: the glow resets on each new counted beat and
  // decays over CI_PULSE_MS. `ciBeat` tracks the last beat we pulsed on.
  const CI_PULSE_MS = 380;
  let ciBeat = 0;
  let ciPulseAt = 0;
  function paint() {
    rafPending = false;
    draw();
    if (get(position).playing) {
      rafPending = true;
      rafId = requestAnimationFrame(paint);
    }
  }
  function requestRedraw() {
    if (rafPending) return;
    rafPending = true;
    rafId = requestAnimationFrame(paint);
  }

  // Repaint whenever any reactive input to draw() changes. While playing,
  // $position ticks keep arriving but the paint loop is already self-sustaining,
  // so requestRedraw is a no-op; while paused, position is steady (server gates
  // unchanged positions) so this settles to zero repaints.
  $effect(() => {
    void $openSong;
    void $position;
    void $selection;
    void $currentLoop;
    void $workingLoop;
    void $allLoopsVisible;
    void $drillSpan;
    void $activeRoutine;
    void $gridVisible;
    void $gridLines;
    void $gridSubdivision;
    void $recordings;
    void view;
    void activeSpan;
    requestRedraw();
  });

  // The canvas reads theme colors at draw time, so an accent change needs an
  // explicit repaint (DOM styled with var(--accent) updates on its own).
  $effect(() => {
    const repaint = () => requestRedraw();
    window.addEventListener(THEME_EVENT, repaint);
    return () => window.removeEventListener(THEME_EVENT, repaint);
  });

  function draw() {
    const ctx = canvas?.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const w = view.width;
    const open = get(openSong);

    // theme colors, read once per frame from a single getComputedStyle (each
    // css() call is its own getComputedStyle, and some reads sit inside
    // per-beat / per-section loops — keep it to one computed-style lookup).
    const cs = getComputedStyle(document.documentElement);
    const v = (name: string) => cs.getPropertyValue(name).trim();
    const c = {
      bg: v("--bg"),
      wave: v("--wave"),
      muted: v("--muted"),
      line: v("--line"),
      accent: v("--accent"),
      accentDim: v("--accent-dim"),
      fg: v("--fg"),
      playhead: v("--playhead"),
      mono: v("--mono"),
    };

    const recs = get(recordings);
    ctx.fillStyle = c.bg;
    ctx.fillRect(0, 0, w, canvasH());

    if (!open) return; // empty state is the .wave-empty HTML overlay

    let playhead = tickPlayhead(playClock, get(position), performance.now());
    // While looping, the engine keeps the playhead inside the region; clamp the
    // interpolated value to the loop so a between-tick extrapolation (or the
    // every-loop count-in drain) can't briefly render it past the loop box.
    const ar0 = get(activeRoutine);
    const loopSpan = get(drillSpan) ?? (ar0?.running ? ar0.block.span : null);
    if (get(position).playing) playhead = clampToLoop(playhead, loopSpan);
    lastPlayhead = playhead;
    // lock viewport to playhead: shift the window before everything else reads
    // `view` this frame, so the wave, lane and playhead all draw against the
    // scrolled window. followView returns the same ref when no shift is needed.
    if (follow && get(position).playing) {
      const next = followView(view, playhead, duration(), FOLLOW_MARGIN);
      if (next !== view) view = next;
    }
    const playheadX = secToX(view, playhead);
    const peaks = open.peaks;
    const perBucket = peaks.frames_per_bucket / SAMPLE_RATE;
    const { first, last } = visibleBuckets(
      view,
      peaks.frames_per_bucket,
      SAMPLE_RATE,
      peaks.buckets.length,
    );

    // waveform: one bar per *device* pixel column. Drawn in device space (no
    // dpr transform) so every bar is exactly 1 physical pixel — a fractional
    // ui_scale (e.g. 2.1, 1.75) would otherwise land 1px CSS bars on sub-pixel
    // boundaries and alias into fixed vertical stripes.
    const wave = c.wave;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    const devW = canvas.width;
    const midDev = (LANE_H + WAVE_H / 2) * dpr;
    const ampDev = (WAVE_H / 2 - 2) * dpr;
    ctx.fillStyle = wave;
    for (let dx = 0; dx < devW; dx++) {
      const b0 = Math.max(Math.floor(xToSec(view, dx / dpr) / perBucket), first);
      const b1 = Math.min(Math.max(Math.floor(xToSec(view, (dx + 1) / dpr) / perBucket), b0), last);
      let lo = Infinity;
      let hi = -Infinity;
      for (let b = b0; b <= b1; b++) {
        const bucket = peaks.buckets[b];
        if (!bucket) continue;
        lo = Math.min(lo, bucket[0]);
        hi = Math.max(hi, bucket[1]);
      }
      if (lo > hi) continue;
      const y0 = midDev - hi * ampDev;
      const y1 = midDev - lo * ampDev;
      ctx.fillRect(dx, y0, 1, Math.max(y1 - y0, 1));
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0); // restore CSS-space for grid/loops/etc.

    // beat grid — subdivision-aware; bottom ticks, or full vertical lines.
    // downbeats render stronger. Hidden when ticks would crowd (< MIN_TICK_PX).
    const analysis = open.analysis;
    if (get(gridVisible) && analysis && analysis.beats.length > 1) {
      const pxPerSec = w / (view.endSec - view.startSec);
      const sub = get(gridSubdivision);
      const times = subdivisionTimes(analysis.beats, analysis.downbeats, sub);
      const span =
        times.length > 1 ? (times[times.length - 1] - times[0]) / (times.length - 1) : Infinity;
      if (span * pxPerSec >= MIN_TICK_PX) {
        const lines = get(gridLines);
        const top = LANE_H;
        const bottom = LANE_H + WAVE_H;
        const downs = new Set(analysis.downbeats);
        for (const t of times) {
          const x = Math.round(secToX(view, t));
          if (x < 0 || x > w) continue;
          const strong = downs.has(t);
          ctx.fillStyle = strong ? c.muted : c.line;
          if (lines) {
            ctx.globalAlpha = strong ? 0.5 : 0.28;
            ctx.fillRect(x, top, 1, WAVE_H);
            ctx.globalAlpha = 1;
          } else {
            const h = strong ? 11 : 6;
            ctx.fillRect(x, bottom - h, 1, h);
          }
        }
      }
    }

    // loop regions — the selected loop (the Delete target) reads boldly: a bright
    // 2px rectangle + denser fill. Unselected loops sit faint. junction edges
    // stay dashed.
    const accent = c.accent;
    const accentDim = c.accentDim;
    const drill = get(drillSpan);
    // only the active loop shows by default; the toggle brings the rest back as
    // a dim overlay (the loops tab always lists them all regardless).
    const showAll = get(allLoopsVisible);
    for (const l of open.loops) {
      const saved = loopBounds(l);
      const isSel = get(currentLoop)?.id === l.id;
      if (!isSel && !showAll) continue;
      // while a loop is active the bold highlight tracks the scratch span (so
      // isolate / run-up are visible); the saved bounds show as a faint ghost
      // when the two diverge, marking where "reset span" returns to.
      const { start, end } = isSel && drill ? drill : saved;
      const diverged = !!(isSel && drill && (drill.start !== saved.start || drill.end !== saved.end));
      const x0 = secToX(view, start);
      const x1 = secToX(view, end);
      if (x1 < 0 || x0 > w) continue;
      ctx.globalAlpha = isSel ? 0.2 : 0.07;
      ctx.fillStyle = accent;
      ctx.fillRect(x0, LANE_H, x1 - x0, WAVE_H);
      ctx.globalAlpha = 1;
      ctx.strokeStyle = isSel ? accent : accentDim;
      ctx.lineWidth = isSel ? 2 : 1;
      const off = isSel ? 1 : 0.5;
      ctx.setLineDash(l.kind.kind === "junction" ? [3, 3] : []);
      ctx.strokeRect(x0 + off, LANE_H + off, x1 - x0 - 2 * off, WAVE_H - 2 * off);
      ctx.setLineDash([]);
      ctx.lineWidth = 1;
      // ghost the saved bounds when the drill span has diverged from them
      if (diverged) {
        const gx0 = secToX(view, saved.start);
        const gx1 = secToX(view, saved.end);
        ctx.strokeStyle = accentDim;
        ctx.lineWidth = 1;
        ctx.setLineDash([2, 4]);
        ctx.strokeRect(gx0 + 0.5, LANE_H + 0.5, gx1 - gx0 - 1, WAVE_H - 1);
        ctx.setLineDash([]);
      }
    }

    // working loop — a live, unsaved loop. Rendered exactly like a selected saved
    // loop (solid, bold): unsaved-ness is signalled by the save glyph in its hover
    // cluster, not by the region's look. Its scratch span drives the highlight;
    // the home bounds ghost when the two diverge (where "reset" returns to). While
    // right-drag resizing it, the preview bounds (in non-reactive `drag`) win.
    const wl = get(workingLoop);
    if (wl) {
      const resizingWorking = drag?.mode === "resize" && drag.loopId === null;
      let start: number;
      let end: number;
      if (drag?.mode === "resize" && drag.loopId === null) {
        start = drag.start;
        end = drag.end;
      } else {
        const b = drill ?? wl;
        start = b.start;
        end = b.end;
      }
      const x0 = secToX(view, start);
      const x1 = secToX(view, end);
      if (x1 >= 0 && x0 <= w) {
        ctx.globalAlpha = 0.2;
        ctx.fillStyle = accent;
        ctx.fillRect(x0, LANE_H, x1 - x0, WAVE_H);
        ctx.globalAlpha = 1;
        ctx.strokeStyle = accent;
        ctx.lineWidth = 2;
        ctx.strokeRect(x0 + 1, LANE_H + 1, x1 - x0 - 2, WAVE_H - 2);
        ctx.lineWidth = 1;
        if (!resizingWorking && drill && (drill.start !== wl.start || drill.end !== wl.end)) {
          const gx0 = secToX(view, wl.start);
          const gx1 = secToX(view, wl.end);
          ctx.strokeStyle = accentDim;
          ctx.setLineDash([2, 4]);
          ctx.strokeRect(gx0 + 0.5, LANE_H + 0.5, gx1 - gx0 - 1, WAVE_H - 1);
          ctx.setLineDash([]);
        }
      }
    }

    // active routine block — while a routine plays there's no saved/working loop,
    // so draw the block the runner is currently looping (bold accent, like a
    // selected loop) so its scale on the wave is obvious.
    const ar = get(activeRoutine);
    if (ar?.running && ar.block) {
      const x0 = secToX(view, ar.block.span.start);
      const x1 = secToX(view, ar.block.span.end);
      if (x1 >= 0 && x0 <= w) {
        ctx.globalAlpha = 0.2;
        ctx.fillStyle = accent;
        ctx.fillRect(x0, LANE_H, x1 - x0, WAVE_H);
        ctx.globalAlpha = 1;
        ctx.strokeStyle = accent;
        ctx.lineWidth = 2;
        ctx.strokeRect(x0 + 1, LANE_H + 1, x1 - x0 - 2, WAVE_H - 2);
        ctx.lineWidth = 1;
      }
    }

    // selection — brighter
    const sel = get(selection);
    if (sel) {
      const x0 = secToX(view, sel.start);
      const x1 = secToX(view, sel.end);
      ctx.globalAlpha = 0.18;
      ctx.fillStyle = accent;
      ctx.fillRect(x0, LANE_H, x1 - x0, WAVE_H);
      ctx.globalAlpha = 1;
      ctx.strokeStyle = c.fg;
      ctx.lineWidth = 1;
      ctx.strokeRect(x0 + 0.5, LANE_H + 0.5, x1 - x0 - 1, WAVE_H - 1);
    }

    // middle-drag zoom preview — dashed accent box over the range to zoom into
    if (drag?.mode === "zoom") {
      const zx0 = Math.min(drag.anchorX, drag.curX);
      const zw = Math.abs(drag.curX - drag.anchorX);
      ctx.globalAlpha = 0.15;
      ctx.fillStyle = accent;
      ctx.fillRect(zx0, LANE_H, zw, WAVE_H);
      ctx.globalAlpha = 1;
      ctx.strokeStyle = accent;
      ctx.setLineDash([4, 3]);
      ctx.strokeRect(zx0 + 0.5, LANE_H + 0.5, Math.max(zw - 1, 0), WAVE_H - 1);
      ctx.setLineDash([]);
    }

    // marker pips — numbered pedal-marker flags hanging just below the section
    // lane, drawn before the playhead so the playhead line stays on top when
    // they coincide. save/restore: font/baseline here are its own, and the
    // structure-lane text pass below relies on the canvas-default baseline.
    ctx.save();
    ctx.font = "8px " + c.mono;
    ctx.textBaseline = "top";
    for (const m of open.markers) {
      const x = secToX(view, m.pos);
      if (x < -MARKER_PIP_W || x > w + MARKER_PIP_W) continue;
      ctx.fillStyle = c.accent;
      ctx.fillRect(x, LANE_H, 1, MARKER_PIP_H); // stem
      ctx.fillRect(x, LANE_H, MARKER_PIP_W, 10); // flag
      ctx.fillStyle = c.bg;
      ctx.fillText(String(m.slot), x + 3, LANE_H + 1);
    }
    ctx.restore();

    // playhead — 1 px white line (its own colour, not the accent, so it reads
    // apart from the loop/selection). Spans only the waveform body: it starts at
    // the bottom of the section-header lane and never runs up into it.
    if (playheadX >= 0 && playheadX <= w) {
      const ci = get(position).countIn;
      if (ci) {
        // held in place, accent-colored, pulsing an area glow on each counted
        // beat — reads as "counting in", visibly distinct from white playback.
        if (ci.beat !== ciBeat) {
          ciBeat = ci.beat;
          ciPulseAt = performance.now();
        }
        const env = Math.max(0, 1 - (performance.now() - ciPulseAt) / CI_PULSE_MS);
        const glow = env * env;
        const x = Math.round(playheadX);
        ctx.save();
        // confine the glow to the waveform body so it doesn't bleed into the lane
        ctx.beginPath();
        ctx.rect(0, LANE_H, w, WAVE_H);
        ctx.clip();
        ctx.shadowColor = c.accent;
        ctx.shadowBlur = 22 * glow;
        ctx.globalAlpha = 0.7 + 0.3 * glow;
        ctx.fillStyle = c.accent;
        ctx.fillRect(x - 1, LANE_H, 3, WAVE_H);
        ctx.restore();
      } else {
        ciBeat = 0;
        ctx.fillStyle = c.playhead;
        ctx.fillRect(Math.round(playheadX), LANE_H, 1, WAVE_H);
      }
    }

    // structure lane — label-colored spans: saved sections solid, analysis
    // suggestions dashed/dimmer/italic; clicked span gets a second fill pass.
    // Hues are derived from the live accent so the lane re-tints with the theme.
    const baseHue = hexToHue(c.accent);
    for (const s of spansFor(open)) {
      const x0 = secToX(view, s.start);
      const x1 = secToX(view, s.end);
      if (x1 < 0 || x0 > w) continue;
      const { fill, edge } = labelColor(s.name, baseHue);
      const active = activeSpan?.start === s.start && activeSpan?.end === s.end;
      // translucent backing so the box dims (but doesn't fully hide) the
      // playhead passing behind it; the tint below is only ~16% opaque.
      ctx.globalAlpha = 0.8;
      ctx.fillStyle = c.bg;
      ctx.fillRect(x0, 2, x1 - x0 - 1, LANE_H - 4);
      ctx.globalAlpha = s.suggested && !active ? 0.6 : 1;
      ctx.fillStyle = fill;
      ctx.fillRect(x0, 2, x1 - x0 - 1, LANE_H - 4);
      if (active) ctx.fillRect(x0, 2, x1 - x0 - 1, LANE_H - 4);
      ctx.strokeStyle = edge;
      ctx.lineWidth = 1;
      ctx.setLineDash(s.suggested ? [3, 3] : []);
      ctx.strokeRect(x0 + 0.5, 2.5, x1 - x0 - 2, LANE_H - 5);
      ctx.setLineDash([]);
      ctx.globalAlpha = 1;
      ctx.fillStyle = c.fg;
      ctx.font = (s.suggested ? "italic " : "") + "11px " + c.mono;
      // sticky label: pin to the visible left edge while any of the span is on
      // screen (but never past its right edge), and truncate against what's left
      const lpad = 4;
      const lx = Math.min(Math.max(x0 + lpad, lpad), x1 - lpad);
      ctx.fillText(s.name, lx, LANE_H - 8, Math.max(x1 - lx - lpad, 0));
    }

    // recording layer lanes — one waveform lane per overdub, stacked beneath the
    // waveform body, time-aligned to the same zoom/scroll. Each take draws its
    // own min/max peaks (mirroring the main waveform), so a silent take reads as
    // a flat line. Muted takes render dimmed. A take whose audio failed to
    // decode (peaks null) draws a baseline so the lane still reads.
    ctx.setLineDash([]); // order-independent: don't inherit a dashed stroke
    for (let i = 0; i < recs.length; i++) {
      const r = recs[i];
      const { start, end } = layerSpanSecs(r.anchor_frame, r.len_frames);
      const x0 = secToX(view, start);
      const x1 = secToX(view, end);
      if (x1 < 0 || x0 > w) continue;
      const laneTop = LANE_H + WAVE_H + i * LAYER_LANE_H;
      const { fill, edge } = labelColor(r.name, baseHue);
      ctx.globalAlpha = r.muted ? 0.4 : 1;
      // lane container: background + border (CSS space)
      ctx.fillStyle = c.bg;
      ctx.fillRect(x0, laneTop + 2, x1 - x0 - 1, LAYER_LANE_H - 4);
      ctx.strokeStyle = edge;
      ctx.lineWidth = 1;
      ctx.strokeRect(x0 + 0.5, laneTop + 2.5, x1 - x0 - 2, LAYER_LANE_H - 5);

      const p = r.peaks;
      if (p && p.buckets.length) {
        // waveform in device space (1px columns), same as the main wave. Bucket
        // b covers this take's own frames [b·fpb, (b+1)·fpb); song time of that
        // bucket is `start + b·fpb/SAMPLE_RATE`, so map each pixel back through
        // `start`.
        const perBucketTake = p.frames_per_bucket / SAMPLE_RATE;
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        const midDev = (laneTop + LAYER_LANE_H / 2) * dpr;
        const ampDev = (LAYER_LANE_H / 2 - 3) * dpr;
        const dxStart = Math.max(0, Math.floor(x0 * dpr));
        const dxEnd = Math.min(canvas.width, Math.ceil(x1 * dpr));
        ctx.fillStyle = fill;
        for (let dx = dxStart; dx < dxEnd; dx++) {
          const t0 = xToSec(view, dx / dpr) - start;
          const t1 = xToSec(view, (dx + 1) / dpr) - start;
          const b0 = Math.max(Math.floor(t0 / perBucketTake), 0);
          const b1 = Math.min(Math.floor(t1 / perBucketTake), p.buckets.length - 1);
          let lo = Infinity;
          let hi = -Infinity;
          for (let b = b0; b <= b1; b++) {
            const bucket = p.buckets[b];
            if (!bucket) continue;
            lo = Math.min(lo, bucket[0]);
            hi = Math.max(hi, bucket[1]);
          }
          if (lo > hi) continue;
          const y0 = midDev - hi * ampDev;
          const y1 = midDev - lo * ampDev;
          ctx.fillRect(dx, y0, 1, Math.max(y1 - y0, 1));
        }
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0); // restore CSS space
      } else {
        // no peaks — a flat baseline so an undecodable take still shows a lane
        ctx.strokeStyle = edge;
        ctx.beginPath();
        ctx.moveTo(x0 + 1, laneTop + LAYER_LANE_H / 2);
        ctx.lineTo(x1 - 1, laneTop + LAYER_LANE_H / 2);
        ctx.stroke();
      }

      // name caption over the lane, left-aligned
      ctx.fillStyle = c.fg;
      ctx.font = "10px " + c.mono;
      const llpad = 4;
      const llx = Math.min(Math.max(x0 + llpad, llpad), x1 - llpad);
      ctx.fillText(r.name, llx, laneTop + LAYER_LANE_H - 4, Math.max(x1 - llx - llpad, 0));
      ctx.globalAlpha = 1;
    }
  }

  function canvasH(): number {
    return LANE_H + WAVE_H + get(recordings).length * LAYER_LANE_H;
  }

  function applySize(w: number, _h: number, dpr: number) {
    if (!canvas) return;
    const h = canvasH();
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    view = { ...view, width: w };
  }

  // Re-apply canvas dimensions when the recording count changes so layer lanes
  // aren't clipped. The canvasSize action fires on container resize; this effect
  // fires when recordings are added/removed. The whole applySize call is
  // untracked: it both reads `view` (the {...view} spread) and writes it, so
  // without untrack this effect would depend on `view` and re-trigger its own
  // write — an effect_update_depth_exceeded loop. The effect therefore depends
  // ONLY on the recording count.
  $effect(() => {
    void $recordings.length;
    untrack(() => applySize(view.width, 0, window.devicePixelRatio || 1));
  });

  onMount(() => {
    requestRedraw(); // initial paint; subsequent ones are demand-driven
    return () => cancelAnimationFrame(rafId);
  });

  // store-reading wrappers around the pure hit-testers in lib/waveform-hit.ts
  /** Topmost loop whose body is under a canvas point (below the lane). */
  function hitLoopBody(x: number, y: number): LoopRegion | null {
    const open = get(openSong);
    return open ? hitBody(open.loops, view, x, y, LANE_H) : null;
  }

  function hitLoopEdge(x: number): LoopEdge | null {
    const open = get(openSong);
    return open ? hitEdge(open.loops, view, x, EDGE_PX) : null;
  }

  /** The loop edge (across all loops) nearest to canvas x. Right-drag grabs this
   *  from anywhere — like Hyprland's super+right-drag snapping to the nearest
   *  tile border instead of requiring a pixel-perfect hit. */
  function nearestLoopEdge(x: number): LoopEdge | null {
    const open = get(openSong);
    return open ? nearestEdge(open.loops, view, x) : null;
  }

  type ResizeTarget = { loopId: number | null; edge: "start" | "end"; start: number; end: number };

  /** Nearest resize target across the working loop AND saved loops (right-drag
   *  grabs from anywhere). `loopId: null` is the working loop. */
  function nearestResizeTarget(x: number): ResizeTarget | null {
    let best: ResizeTarget | null = null;
    let bestDist = Infinity;
    const consider = (loopId: number | null, edge: "start" | "end", start: number, end: number, atSec: number) => {
      const d = Math.abs(secToX(view, atSec) - x);
      if (d < bestDist) ((bestDist = d), (best = { loopId, edge, start, end }));
    };
    const wl = get(workingLoop);
    if (wl) {
      consider(null, "start", wl.start, wl.end, wl.start);
      consider(null, "end", wl.start, wl.end, wl.end);
    }
    const le = nearestLoopEdge(x);
    if (le) consider(le.loop.id, le.edge, le.loop.start, le.loop.end, le.edge === "start" ? le.loop.start : le.loop.end);
    return best;
  }

  /** True when canvas x is within the edge hit-zone of the working loop. */
  function hitWorkingEdge(x: number): boolean {
    const wl = get(workingLoop);
    if (!wl) return false;
    return (
      Math.abs(secToX(view, wl.start) - x) <= EDGE_PX || Math.abs(secToX(view, wl.end) - x) <= EDGE_PX
    );
  }

  function canvasX(e: MouseEvent): number {
    return e.clientX - canvas.getBoundingClientRect().left;
  }

  function canvasY(e: MouseEvent): number {
    return e.clientY - canvas.getBoundingClientRect().top;
  }

  /** Lane span containing a time (used while dragging across headers). */
  function spanAtTime(sec: number): { start: number; end: number } | null {
    const open = get(openSong);
    return open ? spanAt(spansFor(open), sec) : null;
  }

  /** Structure-lane span under a canvas point (lane y-band only). */
  function hitLaneSpan(x: number, y: number): LaneSpan | null {
    const open = get(openSong);
    return open ? hitLane(spansFor(open), view, x, y, LANE_H) : null;
  }

  /** Marker pip under a canvas point (numbered flag, just below the lane). */
  function hitMarkerPip(x: number, y: number): { slot: number; pos: number } | null {
    const open = get(openSong);
    return open ? hitPip(view, open.markers, x, y, LANE_H) : null;
  }

  function onPointerDown(e: PointerEvent) {
    if (!get(openSong)) return;
    const x = canvasX(e);
    // middle button: drag a range to zoom into it (click with no drag = fit)
    if (e.button === 1) {
      e.preventDefault();
      canvas.setPointerCapture(e.pointerId);
      drag = { mode: "zoom", anchorX: x, curX: x };
      return;
    }
    // right button is the ONLY resize: grab the nearest loop edge from anywhere
    // (Hyprland super+right-drag feel) and drag it. inert if there are no loops.
    if (e.button === 2) {
      const t = nearestResizeTarget(x);
      if (t) {
        e.preventDefault();
        canvas.setPointerCapture(e.pointerId);
        drag = { mode: "resize", loopId: t.loopId, edge: t.edge, start: t.start, end: t.end, start0: t.start, end0: t.end };
      }
      return;
    }
    if (e.button !== 0) return;
    // left button = select & drag only — it never resizes.
    // lane click/drag: single click plays the section through; double click (or
    // double-click-drag) loops it — resolved on pointer-up using `double`.
    const span = hitLaneSpan(x, canvasY(e));
    if (span) {
      const double = e.timeStamp - lastLaneDownAt < DBLCLICK_MS;
      lastLaneDownAt = e.timeStamp;
      canvas.setPointerCapture(e.pointerId);
      drag = { mode: "lane", anchor: { start: span.start, end: span.end }, moved: false, double };
      return;
    }
    lastLaneDownAt = 0; // a non-lane click breaks any double-click sequence
    canvas.setPointerCapture(e.pointerId);
    drag = { mode: "select", anchorX: x, moved: false };
  }

  /** Pull a time onto the nearest snap target when grid snap is on. Targets are
   *  the section boundaries (the primary alignment for loops — so dragging a loop
   *  edge lands exactly on the verse/chorus edge) plus the beat/bar grid at the
   *  chosen subdivision. Nearest within `thresholdPx` wins; identity otherwise
   *  (Infinity locks to the nearest target regardless of distance). */
  function maybeSnap(secs: number, thresholdPx = SNAP_PX): number {
    if (!get(gridSnap)) return secs;
    const open = get(openSong);
    if (!open) return secs;
    const targets: number[] = [];
    for (const s of spansFor(open)) {
      targets.push(s.start, s.end);
    }
    const a = open.analysis;
    if (a && a.beats.length) targets.push(...subdivisionTimes(a.beats, a.downbeats, get(gridSubdivision)));
    if (!targets.length) return secs;
    return snapToGrid(secs, targets, view, thresholdPx);
  }

  function onPointerMove(e: PointerEvent) {
    const x = canvasX(e);
    if (!drag) {
      const y = canvasY(e);
      let cursor = "crosshair";
      if (hitLoopEdge(x) || hitWorkingEdge(x)) cursor = "ew-resize";
      else if (hitLaneSpan(x, y)) cursor = "grab";
      else if (hitMarkerPip(x, y) || hitLoopBody(x, y)) cursor = "pointer";
      canvas.style.cursor = cursor;
      return;
    }
    if (drag.mode === "zoom") {
      drag.curX = x;
      requestRedraw(); // zoom preview box lives in non-reactive `drag`
      return;
    }
    if (drag.mode === "lane") {
      const cur = spanAtTime(Math.min(Math.max(xToSec(view, x), 0), duration()));
      if (cur && (cur.start !== drag.anchor.start || cur.end !== drag.anchor.end)) {
        drag.moved = true;
      }
      if (drag.moved) {
        const lo = cur ? Math.min(drag.anchor.start, cur.start) : drag.anchor.start;
        const hi = cur ? Math.max(drag.anchor.end, cur.end) : drag.anchor.end;
        selection.set({ start: lo, end: hi });
      }
      return;
    }
    const secs = maybeSnap(Math.min(Math.max(xToSec(view, x), 0), duration()));
    if (drag.mode === "resize") {
      if (drag.edge === "start") drag.start = Math.min(secs, drag.end - 0.05);
      else drag.end = Math.max(secs, drag.start + 0.05);
      requestRedraw(); // resize bounds live in non-reactive `drag`
    } else {
      if (Math.abs(x - drag.anchorX) >= CLICK_PX) drag.moved = true;
      if (drag.moved) {
        const a = maybeSnap(xToSec(view, drag.anchorX));
        selection.set({ start: Math.min(a, secs), end: Math.max(a, secs) });
      }
    }
  }

  function onPointerUp(e: PointerEvent) {
    const d = drag;
    drag = null;
    if (!d) return;
    if (d.mode === "zoom") {
      const a = xToSec(view, d.anchorX);
      const b = xToSec(view, d.curX);
      const lo = Math.max(0, Math.min(a, b));
      const hi = Math.min(duration(), Math.max(a, b));
      if (hi - lo >= 0.2 && Math.abs(d.curX - d.anchorX) >= CLICK_PX) {
        view = { ...view, startSec: lo, endSec: hi };
      } else {
        view = { ...view, startSec: 0, endSec: Math.max(duration(), 2) };
      }
      return;
    }
    if (d.mode === "lane") {
      if (d.double) {
        // double click (± drag across headers) → SELECT the section / span. No
        // loop is created here; the ⟳ button on the selection does that.
        if (!d.moved) {
          selection.set({ start: d.anchor.start, end: d.anchor.end });
          activeSpan = { start: d.anchor.start, end: d.anchor.end };
        } else {
          activeSpan = null; // multi-section: the selection box shows the range
        }
      } else if (!d.moved) {
        // single click → place the playhead at the section start; active-play
        // also starts playback from there.
        void placePlayhead(d.anchor.start);
      }
      // single-click drag (!double && moved): selection was set during the drag;
      // leave it for the user to loop/save by hand — no auto-loop.
      return;
    }
    if (d.mode === "resize") {
      if (d.start !== d.start0 || d.end !== d.end0) {
        if (d.loopId === null) {
          // working loop: update its bounds in place (no DB round-trip; save will
          // persist these). The store set is synchronous, so no pin is needed.
          void actions.setWorkingLoopBounds(d.start, d.end);
        } else {
          // pin the new bounds visually until the store reflects them, then release
          const id = d.loopId;
          pendingResize = { id, start: d.start, end: d.end };
          requestRedraw(); // pinned bounds live in non-reactive `pendingResize`
          void actions.updateLoop(id, { start: d.start, end: d.end }).finally(() => {
            if (pendingResize?.id === id) pendingResize = null;
            requestRedraw(); // repaint once the store round-trip releases the pin
          });
        }
      }
    } else if (!d.moved) {
      const cx = canvasX(e);
      const cy = canvasY(e);
      // a plain click dismisses only the transient drag-selection box — the active
      // loop (working or saved) is STICKY and never cleared by a click. Clicking a
      // *visible* saved loop's body establishes that one as active; clicking empty
      // space (or while only the active loop shows) leaves it be — just seeks. This
      // applies whether or not the click also lands on a marker pip below.
      selection.set(null);
      // marker pip beats the generic seek below — clicking a numbered flag
      // jumps to that saved position instead of wherever the click landed.
      const pip = hitMarkerPip(cx, cy);
      if (pip) {
        void placePlayhead(pip.pos);
        return;
      }
      const loop = get(allLoopsVisible) ? hitLoopBody(cx, cy) : null;
      if (loop) {
        workingLoop.set(null);
        currentLoop.set(loop);
      }
      // with click snap on, the seek locks to the nearest grid target (no
      // pull radius — a click can't land between snap points)
      const secs = Math.min(Math.max(xToSec(view, cx), 0), duration());
      void placePlayhead(get(clickSnap) ? maybeSnap(secs, Infinity) : secs);
    }
  }

  /** Move the playhead to `secs`; in active-play mode also start playback there
   *  (a click then plays, rather than only repositioning). */
  async function placePlayhead(secs: number): Promise<void> {
    await actions.seek(secs);
    if (get(activePlay) && !get(position).playing) await actions.play();
  }

  function onWheel(e: WheelEvent) {
    if (!get(openSong)) return;
    e.preventDefault();
    if (e.shiftKey) {
      // pan — same clamp as the scrollbar, via the shared window helper. A
      // manual pan means the user is taking control → drop follow.
      follow = false;
      const span = view.endSec - view.startSec;
      const shift = (e.deltaY / view.width) * span;
      const win = adjustWindow("pan", view.startSec + shift, view.endSec + shift, duration(), MIN_WIN);
      view = { ...view, ...win };
    } else {
      // zoom stays follow-aware: anchor on the playhead while following so the
      // zoom can't push it out of the window (else anchor on the cursor).
      const factor = e.deltaY > 0 ? 1.25 : 0.8;
      const anchor = follow ? lastPlayhead : xToSec(view, canvasX(e));
      view = zoom(view, anchor, factor, duration());
    }
  }

  /** Loop the selection: spin up a working (unsaved) loop over it and play —
   *  which opens the drill box. Nothing is persisted until the save glyph. */
  async function loopSelection() {
    const sel = get(selection);
    if (!sel) return;
    selection.set(null);
    await actions.loopSpan(sel.start, sel.end);
  }

  /** Save glyph on a working loop: persist it (or adopt a matching saved loop). */
  function saveWorkingLoop() {
    void actions.saveWorkingLoop();
  }

  /** Loop glyph on a working loop: re-point the transport at it and play. */
  async function playWorkingLoop() {
    const w = get(workingLoop);
    if (!w) return;
    const b = get(drillSpan) ?? w;
    await actions.setTransportLoop(b.start, b.end);
    await actions.seek(b.start);
    await actions.play();
  }

  /** ✕ glyph on a working loop: dismiss it without saving. */
  function discardWorkingLoop() {
    void actions.clearTransportLoop();
  }

  /** Loop glyph on the selected loop: point the transport at it and play. */
  async function playCurrentLoop() {
    const l = get(currentLoop);
    if (!l) return;
    const b = get(drillSpan) ?? loopBounds(l);
    await actions.setTransportLoop(b.start, b.end);
    await actions.seek(b.start);
    await actions.play();
  }

  /** ✕ glyph on the selected loop: delete it (clears the transport loop too). */
  async function deleteCurrentLoop() {
    const l = get(currentLoop);
    if (l) await actions.deleteLoop(l.id);
  }

  /** Empty-state "library" link: bring the library tab to the front, expanding
   *  its region if collapsed. */
  function openLibrary() {
    void actions.revealTab("library");
  }

  // Cursor position in waveform px (or null off-canvas) — drives the hover-reveal
  // action clusters for the selection and the selected loop.
  let hoverPt = $state<{ x: number; y: number } | null>(null);

  // When the cursor is over the selection, the selection owns the hover — the
  // loop's controls must NOT reveal where the two overlap. The loop clusters use
  // this gated pointer (null over the selection) instead of the raw hoverPt.
  let loopPointer = $derived.by(() => {
    const sel = $selection;
    if (sel && hoverPt) {
      const a = secToX(view, sel.start);
      const b = secToX(view, sel.end);
      if (hoverPt.x >= Math.min(a, b) && hoverPt.x <= Math.max(a, b)) return null;
    }
    return hoverPt;
  });

  let dur = $derived($openSong?.song.duration_secs ?? 0);
  const MIN_WIN = 1; // seconds — min visible window

  let scrollEl: HTMLDivElement;
  type ScrollDrag = { mode: "pan" | "start" | "end"; px0: number; s0: number; e0: number };
  let scrollDrag: ScrollDrag | null = null;

  function scrollPx(e: PointerEvent): number {
    const rect = scrollEl.getBoundingClientRect();
    return e.clientX - rect.left;
  }
  function pxToDsec(dpx: number): number {
    const w = scrollEl?.clientWidth || 1;
    return (dpx / w) * dur;
  }
  function onScrollDown(e: PointerEvent) {
    if (dur <= 0) return;
    follow = false; // any scrollbar pan/resize/recenter is a manual override
    const w = scrollEl.clientWidth;
    const px = scrollPx(e);
    const x0 = (view.startSec / dur) * w;
    const x1 = (view.endSec / dur) * w;
    // right button is the ONLY resize: grab the nearest window edge from anywhere
    // (Hyprland super+right-drag feel) and drag it.
    if (e.button === 2) {
      const mode = Math.abs(px - x0) <= Math.abs(px - x1) ? "start" : "end";
      e.preventDefault();
      scrollEl.setPointerCapture(e.pointerId);
      scrollDrag = { mode, px0: px, s0: view.startSec, e0: view.endSec };
      return;
    }
    if (e.button !== 0) return;
    // left = pan the window (drag) or recenter (click outside) — never resizes.
    if (px >= x0 && px <= x1) {
      scrollEl.setPointerCapture(e.pointerId);
      scrollDrag = { mode: "pan", px0: px, s0: view.startSec, e0: view.endSec };
    } else {
      // click outside the window: recenter the window there (keep width)
      const width = view.endSec - view.startSec;
      const c = (px / w) * dur;
      const win = adjustWindow("pan", c - width / 2, c + width / 2, dur, MIN_WIN);
      view = { ...view, startSec: win.startSec, endSec: win.endSec };
    }
  }
  function onScrollMove(e: PointerEvent) {
    if (!scrollDrag) {
      // hover feedback: resize cursor over the window edges, grab over its body
      if (dur <= 0) return;
      const w = scrollEl.clientWidth;
      const px = scrollPx(e);
      const x0 = (view.startSec / dur) * w;
      const x1 = (view.endSec / dur) * w;
      const EDGE = 6;
      scrollEl.style.cursor =
        Math.abs(px - x0) <= EDGE || Math.abs(px - x1) <= EDGE
          ? "ew-resize"
          : px > x0 && px < x1
            ? "grab"
            : "default";
      return;
    }
    const d = pxToDsec(scrollPx(e) - scrollDrag.px0);
    let s = scrollDrag.s0;
    let en = scrollDrag.e0;
    if (scrollDrag.mode === "pan") { s += d; en += d; }
    else if (scrollDrag.mode === "start") { s += d; }
    else { en += d; }
    const win = adjustWindow(scrollDrag.mode, s, en, dur, MIN_WIN);
    view = { ...view, startSec: win.startSec, endSec: win.endSec };
  }
  function onScrollUp() { scrollDrag = null; }
  function resetView() { view = { ...view, startSec: 0, endSec: dur }; }
</script>

<!-- pointer tracked at the container (not the canvas) so moving onto the overlay
     action buttons — children of .waveform — never reads as leaving, which would
     flicker the hover clusters at the canvas/button seam -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="waveform"
  use:canvasSize={applySize}
  onpointermove={(e) => (hoverPt = { x: canvasX(e), y: canvasY(e) })}
  onpointerleave={() => (hoverPt = null)}
>
  <canvas
    id="waveform-canvas"
    bind:this={canvas}
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onwheel={onWheel}
    oncontextmenu={(e) => e.preventDefault()}
  ></canvas>
  {#if !$openSong}
    <div class="wave-empty" style="top: {LANE_H}px; height: {WAVE_H}px;">
      {#if $openingSong !== null}
        <span class="we-title">opening…</span>
      {:else}
        <svg
          class="we-glyph"
          width="40"
          height="26"
          viewBox="0 0 40 26"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
        >
          <line x1="4" y1="11" x2="4" y2="15" />
          <line x1="9" y1="8" x2="9" y2="18" />
          <line x1="14" y1="3" x2="14" y2="23" />
          <line x1="19" y1="9" x2="19" y2="17" />
          <line x1="24" y1="5" x2="24" y2="21" />
          <line x1="29" y1="10" x2="29" y2="16" />
          <line x1="34" y1="7" x2="34" y2="19" />
        </svg>
        <span class="we-title">no song open</span>
        <span class="we-hint">
          pick a track in the <button class="we-link" onclick={openLibrary}>library</button>
        </span>
      {/if}
    </div>
  {/if}
  {#if $openSong?.analysis && (hoverPt || gridOpen)}
    <div class="grid-ctl" class:open={gridOpen} transition:fade={{ duration: 120 }}>
      <div class="grid-fields">
        <button class:on={$gridSnap} onclick={() => void actions.setGridSnap(!$gridSnap)} title="snap to grid (g)">snap</button>
        <button class:on={$gridVisible} onclick={() => void actions.setGridVisible(!$gridVisible)} title="show grid">grid</button>
        <button class:on={$gridLines} onclick={() => void actions.setGridLines(!$gridLines)} title="full gridlines vs bottom ticks">lines</button>
        <span class="seg">
          {#each GRID_SUBDIVS as s (s)}
            <button class:on={$gridSubdivision === s} onclick={() => void actions.setGridSubdivision(s)}>{s}</button>
          {/each}
        </span>
      </div>
      <button
        class="grid-toggle"
        class:on={gridOpen}
        onclick={() => (gridOpen = !gridOpen)}
        title={gridOpen ? "hide grid controls" : "grid controls"}
        aria-label="grid controls"
        aria-pressed={gridOpen}
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
          <line x1="4" y1="9.5" x2="20" y2="9.5" />
          <line x1="4" y1="14.5" x2="20" y2="14.5" />
          <line x1="9.5" y1="4" x2="9.5" y2="20" />
          <line x1="14.5" y1="4" x2="14.5" y2="20" />
        </svg>
      </button>
    </div>
  {/if}
  {#if $openSong && (hoverPt || follow)}
    <!-- lock viewport to playhead: hover-revealed, stays lit (cyan) while on -->
    <button
      class="follow-toggle"
      class:on={follow}
      style="top: {LANE_H + 4}px;"
      onclick={() => (follow = !follow)}
      title={follow ? "following playhead — click to unlock" : "lock viewport to playhead"}
      aria-label="lock viewport to playhead"
      aria-pressed={follow}
      transition:fade={{ duration: 120 }}
    >
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1" stroke-linecap="round" aria-hidden="true">
        <circle cx="12" cy="12" r="5.5" />
        <line x1="12" y1="2.5" x2="12" y2="21.5" />
      </svg>
    </button>
  {/if}
  <div
    class="scrollbar"
    role="scrollbar"
    aria-label="waveform range selector"
    aria-controls="waveform-canvas"
    aria-valuenow={Math.round((view.startSec / (dur || 1)) * 100)}
    aria-valuemin={0}
    aria-valuemax={100}
    tabindex="0"
    bind:this={scrollEl}
    onpointerdown={onScrollDown}
    onpointermove={onScrollMove}
    onpointerup={onScrollUp}
    onpointercancel={onScrollUp}
    ondblclick={resetView}
    oncontextmenu={(e) => e.preventDefault()}
    title="left-drag to scroll · right-drag to zoom (grabs nearest edge) · double-click to fit"
  >
    {#if dur > 0}
      <div
        class="sb-window"
        style="left: {(view.startSec / dur) * 100}%; width: {((view.endSec - view.startSec) / dur) * 100}%"
      ></div>
    {/if}
  </div>
  {#if $openingSong !== null && $openSong}
    <!-- song switch in flight: keep the old waveform, show progress on top -->
    <div class="loading-bar"></div>
  {/if}
  {#if $selection}
    <HoverActions
      left={secToX(view, $selection.start)}
      right={secToX(view, $selection.end)}
      bandTop={LANE_H}
      bandHeight={WAVE_H}
      viewWidth={view.width}
      pointer={hoverPt}
      count={1}
      z={4}
    >
      <button class="sa-btn" onclick={loopSelection} title="loop — opens the drill (not saved until you save it)" aria-label="loop selection">⟳</button>
    </HoverActions>
  {/if}
  {#if $workingLoop}
    <HoverActions
      left={secToX(view, ($drillSpan ?? $workingLoop).start)}
      right={secToX(view, ($drillSpan ?? $workingLoop).end)}
      bandTop={LANE_H}
      bandHeight={WAVE_H}
      viewWidth={view.width}
      pointer={loopPointer}
      count={3}
    >
      <button class="sa-btn" onclick={saveWorkingLoop} title="save loop" aria-label="save working loop">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M7 4h10v16l-5-3.5-5 3.5z" />
        </svg>
      </button>
      <button class="sa-btn" onclick={playWorkingLoop} title="play loop" aria-label="play working loop">⟳</button>
      <button class="sa-btn danger" onclick={discardWorkingLoop} title="discard — don't save" aria-label="discard working loop">✕</button>
    </HoverActions>
  {/if}
  {#if $currentLoop}
    <HoverActions
      left={secToX(view, ($drillSpan ?? loopBounds($currentLoop)).start)}
      right={secToX(view, ($drillSpan ?? loopBounds($currentLoop)).end)}
      bandTop={LANE_H}
      bandHeight={WAVE_H}
      viewWidth={view.width}
      pointer={loopPointer}
      count={2}
    >
      <button class="sa-btn" onclick={playCurrentLoop} title="play loop" aria-label="play loop">⟳</button>
      <button class="sa-btn danger" onclick={deleteCurrentLoop} title="delete loop" aria-label="delete loop">✕</button>
    </HoverActions>
  {/if}
</div>

<style>
  .waveform {
    position: relative;
    width: 100%;
  }

  canvas {
    display: block;
    cursor: crosshair;
  }

  /* grid control overlay — lower-right of the canvas, above the scrollbar
     (kept clear of the section lane along the top) */
  .grid-ctl {
    position: absolute;
    bottom: 20px;
    right: 4px;
    display: flex;
    align-items: center;
    padding: 3px;
    background: color-mix(in srgb, var(--bg) 80%, transparent);
    border: 1px solid var(--line);
    border-radius: var(--radius);
  }

  /* collapsed: fields shrink to nothing behind the corner toggle; opening
     extends them leftward with a smooth width + fade */
  .grid-fields {
    display: flex;
    align-items: center;
    gap: 4px;
    max-width: 0;
    margin-right: 0;
    opacity: 0;
    overflow: hidden;
    white-space: nowrap;
    transition:
      max-width 0.22s ease,
      margin-right 0.22s ease,
      opacity 0.18s ease;
  }
  .grid-ctl.open .grid-fields {
    max-width: 320px;
    margin-right: 4px;
    opacity: 1;
  }

  .grid-fields button {
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--muted);
    font-size: 10px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    padding: 1px 5px;
    cursor: pointer;
  }
  .grid-fields button:hover {
    color: var(--fg);
  }
  .grid-fields button.on {
    color: var(--accent);
    border-color: var(--accent-dim);
  }
  .grid-fields .seg {
    display: inline-flex;
    gap: 2px;
    border-left: 1px solid var(--line);
    padding-left: 4px;
    margin-left: 1px;
  }

  .grid-toggle {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 2px;
    line-height: 0;
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--muted);
    cursor: pointer;
  }
  .grid-toggle svg {
    width: 15px;
    height: 15px;
  }
  .grid-toggle:hover {
    color: var(--fg);
  }
  .grid-toggle.on {
    color: var(--accent);
    border-color: var(--accent-dim);
  }

  .follow-toggle {
    position: absolute;
    right: 4px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 3px;
    line-height: 0;
    background: color-mix(in srgb, var(--bg) 80%, transparent);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    color: var(--muted);
    cursor: pointer;
  }
  .follow-toggle svg {
    width: 16px;
    height: 16px;
  }
  .follow-toggle:hover {
    color: var(--fg);
  }
  .follow-toggle.on {
    color: var(--accent);
    border-color: var(--accent-dim);
  }

  /* thin indeterminate bar across the top of the stage while a new song
     decodes — same accent + timing language as the prepare modal's bar */
  .loading-bar {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 2px;
    overflow: hidden;
  }

  .loading-bar::after {
    content: "";
    position: absolute;
    width: 30%;
    height: 100%;
    background: var(--accent);
    animation: indeterminate 1s ease-in-out infinite;
  }

  @keyframes indeterminate {
    from {
      left: -30%;
    }
    to {
      left: 100%;
    }
  }

  /* glyph buttons rendered inside HoverActions (selection + selected loop) */
  .sa-btn {
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 13px;
    line-height: 1;
    color: var(--fg);
    background: color-mix(in srgb, var(--bg) 80%, transparent);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    cursor: pointer;
    padding: 0;
  }
  .sa-btn svg {
    width: 14px;
    height: 14px;
  }
  .sa-btn:hover {
    color: var(--accent);
  }
  .sa-btn.danger:hover {
    color: var(--shaky);
  }

  /* empty state, centered over the wave area (canvas draws nothing when no song) */
  .wave-empty {
    position: absolute;
    left: 0;
    right: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    text-align: center;
    /* let pointer tracking / interactions fall through to the canvas; only the
       link opts back in */
    pointer-events: none;
  }
  .we-glyph {
    color: var(--muted);
    opacity: 0.5;
  }
  .we-title {
    color: var(--muted);
    font-size: 14px;
    letter-spacing: 0.04em;
  }
  .we-hint {
    color: var(--muted);
    opacity: 0.75;
    font-size: 11px;
  }
  .we-link {
    pointer-events: auto;
    background: none;
    border: none;
    font: inherit;
    color: var(--accent-dim);
    cursor: pointer;
    padding: 0;
    text-decoration: underline;
  }
  .we-link:hover {
    color: var(--accent);
  }

  .scrollbar {
    position: relative;
    height: 12px;
    margin-top: 4px;
    background: var(--bg-raised);
    border-radius: 3px;
    cursor: pointer;
    user-select: none;
  }

  .sb-window {
    position: absolute;
    top: 0;
    bottom: 0;
    min-width: 6px;
    background: var(--accent);
    opacity: 0.35;
    border-radius: 3px;
    box-sizing: border-box;
  }
</style>
