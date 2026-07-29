<script lang="ts">
  // Durable settings — every control writes through to the server-side
  // settings table immediately; the UI-scale fader previews live but only
  // applies the zoom on release (a re-zoom + DB write per pixel is too heavy).
  import {
    actions,
    ANALYSIS_DEVICE,
    clickSnap,
    COLOR_THEME,
    GRID_SNAP_DEFAULT,
    gridSnap,
    LIBRARY_ROOT,
    openSong,
    prepareState,
    settings,
    UI_SCALE,
    WINDOW_DECORATIONS,
  } from "../lib/stores";
  import { ACCENTS, applyTheme } from "../lib/theme";
  import Button from "../lib/ui/Button.svelte";
  import Fader from "../lib/ui/Fader.svelte";
  import SectionHead from "../lib/ui/SectionHead.svelte";
  import { cmd } from "../lib/ipc";
  import { applyDecorations } from "../lib/window";
  import { getZoom, setZoom } from "../lib/zoom";
  import { onMount } from "svelte";

  // Optional external tools that gate features. The backend `caps` command
  // probes for each; we show which features are live and a full/partial summary.
  type Caps = { mp3: boolean; stems: boolean; analysis: boolean };
  const FEATURES: { key: keyof Caps; label: string; dep: string }[] = [
    { key: "stems", label: "stem separation", dep: "demucs" },
    { key: "analysis", label: "structure analysis", dep: "analyze (PyTorch)" },
    { key: "mp3", label: "MP3 export", dep: "ffmpeg" },
  ];
  let caps = $state<Caps>({ mp3: false, stems: false, analysis: false });
  let ready = $derived(FEATURES.filter((f) => caps[f.key]).length);
  let capSummary = $derived(
    ready === FEATURES.length
      ? "full capability — every optional feature is available"
      : ready === 0
        ? "core only — looping, stretch & tuner work; optional tools are missing"
        : `partial capability — ${ready} of ${FEATURES.length} optional features available`,
  );

  onMount(() => {
    void cmd<Caps>("caps").then((c) => (caps = c));
  });

  let scale = $derived(Number($settings[UI_SCALE] ?? getZoom()));
  // live preview while dragging; zoom is only applied on release (a full
  // webview re-zoom + DB write is too heavy to run on every pixel)
  let preview = $state<number | null>(null);
  let shownScale = $derived(preview ?? scale);
  let snapDefault = $derived($settings[GRID_SNAP_DEFAULT] !== false);
  let device = $derived(($settings[ANALYSIS_DEVICE] as string) ?? "auto");
  // Defaults a fresh tablature block starts at; both optional (fall back 4 / 16).
  let tabStrings = $derived(Number($settings["default_tab_strings"] ?? 4));
  let tabWidth = $derived(Number($settings["default_tab_width"] ?? 16));
  // Library location override; empty means the OS default. Write-through trims;
  // a blank value clears the override. Takes effect on the next launch.
  let libraryRoot = $derived(($settings[LIBRARY_ROOT] as string) ?? "");

  function setLibraryRoot(raw: string) {
    void actions.setSetting(LIBRARY_ROOT, raw.trim());
  }

  function setTabDefault(key: string, raw: string, lo: number, hi: number, fallback: number) {
    const n = Math.round(Number(raw));
    const clamped = Number.isFinite(n) ? Math.max(lo, Math.min(hi, n)) : fallback;
    void actions.setSetting(key, clamped);
  }
  // default on: only an explicit false hides the frame
  let decorations = $derived($settings[WINDOW_DECORATIONS] !== false);
  let themeValue = $derived(
    typeof $settings[COLOR_THEME] === "string" ? ($settings[COLOR_THEME] as string) : "amber",
  );
  let activeName = $derived(ACCENTS.find((o) => o.value === themeValue)?.name ?? themeValue);

  function setAccent(value: string) {
    applyTheme(value); // live swap
    void actions.setSetting(COLOR_THEME, value);
  }

  function toggleSnap() {
    const next = !snapDefault;
    void actions.setSetting(GRID_SNAP_DEFAULT, next);
    gridSnap.set(next); // apply to the running session too
  }

  function toggleDecorations() {
    const next = !decorations;
    void applyDecorations(next); // apply to the live window immediately
    void actions.setSetting(WINDOW_DECORATIONS, next);
  }
</script>

<section class="group">
  <SectionHead>appearance</SectionHead>

  <div class="setting stacked">
    <div class="text"><span class="name">ui scale</span></div>
    <div class="fader-row">
      <Fader
        value={shownScale}
        min={0.75}
        max={2.5}
        step={0.05}
        accent
        onchange={(v) => (preview = v)}
        oncommit={(v) => {
          preview = null;
          void setZoom(v);
        }}
        format={(v) => `ui scale ${Math.round(v * 100)}%`}
      />
      <span class="readout mono">{Math.round(shownScale * 100)}%</span>
    </div>
  </div>

  <div class="setting stacked">
    <div class="text"><span class="name">color theme</span></div>
    <div class="swatches">
      {#each ACCENTS as opt (opt.value)}
        <button
          class="swatch-btn"
          class:active={themeValue === opt.value}
          style="--dot: {opt.hex}"
          onclick={() => setAccent(opt.value)}
          title={opt.name}
          aria-label={opt.name}
        >
          <span class="dot"></span>
        </button>
      {/each}
      <span class="swatch-name">{activeName}</span>
    </div>
  </div>

  <div class="setting">
    <div class="text">
      <span class="name">native window frame</span>
      <span class="desc">the OS title bar + min/max/close · off is borderless (use your WM)</span>
    </div>
    <Button variant="toggle" active={decorations} onclick={toggleDecorations}>
      {decorations ? "on" : "off"}
    </Button>
  </div>
</section>

<section class="group">
  <SectionHead>editing</SectionHead>

  <div class="setting">
    <div class="text">
      <span class="name">grid snap by default</span>
      <span class="desc">loop + selection edges snap to analyzed downbeats</span>
    </div>
    <Button variant="toggle" active={snapDefault} onclick={toggleSnap}>
      {snapDefault ? "on" : "off"}
    </Button>
  </div>

  <div class="setting">
    <div class="text">
      <span class="name">single click snaps</span>
      <span class="desc">clicks lock to the nearest grid line while snap is on · off = click anywhere</span>
    </div>
    <Button variant="toggle" active={$clickSnap} onclick={() => void actions.setClickSnap(!$clickSnap)}>
      {$clickSnap ? "on" : "off"}
    </Button>
  </div>
</section>

<section class="group">
  <SectionHead>library</SectionHead>

  <div class="setting stacked">
    <div class="text">
      <span class="name">library folder</span>
      <span class="desc">where song bundles are stored · blank = default · applies on restart</span>
    </div>
    <input
      class="path"
      type="text"
      placeholder="~/Music/dredge"
      value={libraryRoot}
      onchange={(e) => setLibraryRoot(e.currentTarget.value)}
    />
  </div>
</section>

<section class="group">
  <SectionHead>notes</SectionHead>

  <div class="setting">
    <div class="text">
      <span class="name">default tab strings</span>
      <span class="desc">rows a new tablature block starts with (1–12)</span>
    </div>
    <input
      class="num"
      type="number"
      min="1"
      max="12"
      value={tabStrings}
      onchange={(e) => setTabDefault("default_tab_strings", e.currentTarget.value, 1, 12, 4)}
    />
  </div>

  <div class="setting">
    <div class="text">
      <span class="name">default tab width</span>
      <span class="desc">columns a new tablature block starts with (1–256)</span>
    </div>
    <input
      class="num"
      type="number"
      min="1"
      max="256"
      value={tabWidth}
      onchange={(e) => setTabDefault("default_tab_width", e.currentTarget.value, 1, 256, 16)}
    />
  </div>
</section>

<section class="group">
  <SectionHead>analysis</SectionHead>

  <div class="setting stacked">
    <div class="text">
      <span class="name">analysis device</span>
      <span class="desc">auto = GPU when it fits, else CPU · cpu = slower, never out of VRAM</span>
    </div>
    <div class="chips">
      <Button
        variant="chip"
        active={device === "auto"}
        onclick={() => void actions.setSetting(ANALYSIS_DEVICE, "auto")}
      >
        auto
      </Button>
      <Button
        variant="chip"
        active={device === "cpu"}
        onclick={() => void actions.setSetting(ANALYSIS_DEVICE, "cpu")}
      >
        cpu
      </Button>
    </div>
  </div>

  <div class="setting">
    <div class="text">
      <span class="name">stem separation</span>
      <span class="desc">re-separate the open song · replaces its existing stems</span>
    </div>
    <Button
      disabled={!$openSong || $prepareState !== null}
      onclick={() => void actions.prepare({ forceStems: true })}
    >
      separate stems
    </Button>
  </div>
</section>

<section class="group">
  <SectionHead>capabilities</SectionHead>

  <div class="cap-summary" class:full={ready === FEATURES.length}>
    <span class="cap-dot" class:on={ready === FEATURES.length}></span>
    {capSummary}
  </div>

  {#each FEATURES as f (f.key)}
    <div class="setting">
      <div class="text">
        <span class="name">{f.label}</span>
        <span class="desc">needs <code>{f.dep}</code></span>
      </div>
      <span class="cap-status" class:ok={caps[f.key]}>
        {caps[f.key] ? "ready" : "missing"}
      </span>
    </div>
  {/each}
</section>

<style>
  .group {
    margin-bottom: calc(var(--space) * 2.5);
  }
  .group:last-child {
    margin-bottom: 0;
  }

  /* one setting: label/desc text block + its control, inline by default */
  .setting {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space);
    padding: 7px 0;
    min-width: 0;
  }

  /* continuous / multi-option controls drop below their label, full width */
  .setting.stacked {
    flex-direction: column;
    align-items: stretch;
    gap: 7px;
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .name {
    font-size: 13px;
    color: var(--fg);
  }
  .desc {
    font-size: 11px;
    color: var(--muted);
    line-height: 1.4;
  }

  .fader-row {
    display: flex;
    align-items: center;
    gap: var(--space);
  }
  .readout {
    flex: 0 0 auto;
    width: 4ch;
    text-align: right;
    font-size: 11px;
  }

  .chips {
    display: flex;
    gap: calc(var(--space) / 2);
    flex-wrap: wrap;
    min-width: 0;
  }

  /* compact numeric setting input (tab defaults) */
  .num {
    flex: 0 0 auto;
    width: 4.5em;
    font: inherit;
    font-size: 12px;
    text-align: right;
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 3px 6px;
  }
  .num:focus-visible {
    outline: 1px solid var(--accent-dim);
    outline-offset: -1px;
  }

  /* full-width path input (library folder) */
  .path {
    width: 100%;
    box-sizing: border-box;
    font: inherit;
    font-size: 12px;
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 5px 8px;
  }
  .path:focus-visible {
    outline: 1px solid var(--accent-dim);
    outline-offset: -1px;
  }

  /* curated accent palette — colour dots, active gets a neutral ring */
  .swatches {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .swatch-btn {
    width: 22px;
    height: 22px;
    padding: 0;
    border: 1px solid transparent;
    border-radius: 50%;
    background: none;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .swatch-btn:hover {
    border-color: var(--line);
  }
  .swatch-btn.active {
    border-color: var(--fg);
  }
  .dot {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--dot);
  }
  .swatch-name {
    margin-left: 2px;
    font-size: 11px;
    color: var(--muted);
  }

  /* capabilities: a status pill per optional tool + a full/partial summary */
  .cap-summary {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 0;
    font-size: 11px;
    line-height: 1.4;
    color: var(--muted);
  }
  .cap-summary.full {
    color: var(--fg);
  }
  .cap-dot {
    flex: 0 0 auto;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.5;
  }
  .cap-dot.on {
    background: var(--accent);
    opacity: 1;
  }
  .cap-status {
    flex: 0 0 auto;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 1px 7px;
  }
  .cap-status.ok {
    color: var(--accent);
    border-color: var(--accent-dim);
  }
  .desc code {
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--fg);
  }
</style>
