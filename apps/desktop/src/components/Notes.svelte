<script lang="ts">
  // The notes box for the active section. Two modes:
  //   - display (default): read-only, tight — empty blocks omitted, no chrome;
  //     follows the playhead (hybrid resolver in lib/active-section).
  //   - edit: full editor (textareas, + text / + tab, right-drag tab resize,
  //     delete); pins the section you entered on so it can't switch mid-edit,
  //     saving + dropping the pin when you leave.
  // A note is an ordered, flexible list of text and tab blocks (no fixed shape).
  // Edits debounce-autosave via actions.setSectionNotes; orphans surface below.
  import { activeLabel } from "../lib/active-section";
  import { emptyTab, type NotesDoc, type TabBlock } from "../lib/notes-doc";
  import { actions, openSong, position, selection, settings } from "../lib/stores";
  import Box from "../lib/ui/Box.svelte";
  import Button from "../lib/ui/Button.svelte";
  import TabBlockView from "./TabBlock.svelte";

  let mode = $state<"display" | "edit">("display");
  let editPin = $state<string | null>(null);
  let pinned = $state<string | null>(null); // display-mode selection pin
  let showOrphans = $state(false);

  let spans = $derived(
    ($openSong?.sections ?? [])
      .filter((s) => s.label)
      .map((s) => ({ label: s.label as string, start: s.start, end: s.end })),
  );

  // In edit mode the pinned section wins; in display we follow the playhead
  // (unless a selection pinned one).
  let active = $derived(
    mode === "edit" && editPin ? editPin : activeLabel(spans, $position.secs, pinned),
  );
  let activeSection = $derived($openSong?.sections.find((s) => s.label === active) ?? null);

  // Local edit buffer; mirrors the stored doc except while editing.
  let doc = $state<NotesDoc>({ blocks: [] });
  // Deliberately non-reactive: effect-local latch so reseeding doesn't retrigger
  // its own effect. Do NOT make this $state.
  let bufferedLabel: string | null = null;

  function clone(d: NotesDoc): NotesDoc {
    return JSON.parse(JSON.stringify(d));
  }

  /** A doc with at least one text block, so a fresh section has a place to type. */
  function seedFrom(stored: NotesDoc | null | undefined): NotesDoc {
    return clone(stored && stored.blocks.length ? stored : { blocks: [{ kind: "text", text: "" }] });
  }

  // Reseed the buffer when the active section changes — but never while editing
  // (would clobber in-progress edits).
  $effect(() => {
    const label = active;
    if (label !== bufferedLabel && mode !== "edit") {
      bufferedLabel = label ?? null;
      doc = seedFrom(activeSection?.notes);
    }
  });

  // Display-mode: clicking a section (waveform / structure tab) pins it.
  $effect(() => {
    const sel = $selection;
    if (!sel) return;
    const hit = spans.find((s) => Math.abs(s.start - sel.start) < 0.05 && Math.abs(s.end - sel.end) < 0.05);
    if (hit) pinned = hit.label;
  });

  // Release the display selection pin on the rising edge of playback so the box
  // resumes following the playhead. Edge-triggered: `$position` is a fresh
  // object every ~50ms tick, so a level check would fire continuously.
  let wasPlaying = false;
  $effect(() => {
    const playing = $position.playing;
    if (playing && !wasPlaying) pinned = null;
    wasPlaying = playing;
  });

  // --- autosave (captures label+doc when the edit happens) --------------------
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let pending: { label: string; snapshot: NotesDoc } | null = null;

  function commit() {
    if (saveTimer) {
      clearTimeout(saveTimer);
      saveTimer = null;
    }
    if (pending) {
      void actions.setSectionNotes(pending.label, pending.snapshot);
      pending = null;
    }
  }
  function queueSave() {
    // Always target the pinned section being edited — never the live `active`,
    // so a save can't be misrouted by a section change or mode switch.
    const label = editPin;
    if (!label) return;
    if (pending && pending.label !== label) commit();
    pending = { label, snapshot: clone(doc) };
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(commit, 500);
  }

  // --- mode toggle ------------------------------------------------------------
  function enterEdit() {
    if (!active) return;
    editPin = active;
    bufferedLabel = active;
    doc = seedFrom(activeSection?.notes);
    mode = "edit";
  }
  function exitEdit() {
    commit();
    mode = "display";
    editPin = null;
  }

  // --- block editing ----------------------------------------------------------
  function editText(i: number, text: string) {
    const blocks = doc.blocks.slice();
    blocks[i] = { kind: "text", text };
    doc = { ...doc, blocks };
    queueSave();
  }
  function editTab(i: number, b: TabBlock) {
    const blocks = doc.blocks.slice();
    blocks[i] = b;
    doc = { ...doc, blocks };
    queueSave();
  }
  function deleteBlock(i: number) {
    doc = { ...doc, blocks: doc.blocks.filter((_, j) => j !== i) };
    queueSave();
  }
  function addTab() {
    const strings = Number($settings["default_tab_strings"] ?? 4);
    const width = Number($settings["default_tab_width"] ?? 16);
    doc = { ...doc, blocks: [...doc.blocks, emptyTab(strings, width)] };
    queueSave();
  }
  function addText() {
    doc = { ...doc, blocks: [...doc.blocks, { kind: "text", text: "" }] };
    queueSave();
  }

  // Display-mode: drop empty text blocks so the read view stays tight.
  let displayBlocks = $derived(
    doc.blocks.filter((b) => b.kind === "tab" || (b.kind === "text" && b.text.trim().length > 0)),
  );
</script>

{#if $openSong && spans.length > 0}
  <Box
    id="notes"
    label={active ? `notes — ${active}` : undefined}
    wide
    showBody={mode === "edit" || displayBlocks.length > 0 || $openSong.orphan_notes.length > 0}
  >
    {#snippet tools()}
      {#if mode === "edit"}
        <button onclick={exitEdit} title="done editing — save & view" aria-label="done editing">done</button>
      {:else}
        <button onclick={enterEdit} title="edit notes for this section" aria-label="edit notes">✎ edit</button>
      {/if}
    {/snippet}

    {#if mode === "edit"}
      <div class="doc">
        {#each doc.blocks as block, i (i)}
          {#if block.kind === "text"}
            <div class="text-row">
              <textarea
                class="text mono"
                value={block.text}
                placeholder={`notes for ${active}…`}
                onblur={() => commit()}
                oninput={(e) => editText(i, e.currentTarget.value)}
              ></textarea>
              <button class="del" onclick={() => deleteBlock(i)} title="delete text" aria-label="delete text">×</button>
            </div>
          {:else}
            <TabBlockView
              block={block as TabBlock}
              onchange={(b) => editTab(i, b)}
              ondelete={() => deleteBlock(i)}
            />
          {/if}
        {/each}
        <div class="inserter">
          <button onclick={addText} title="add a text block">+ text</button>
          <button onclick={addTab} title="add a tablature block">+ tab</button>
        </div>
      </div>
    {:else if displayBlocks.length > 0}
      <div class="doc display">
        {#each displayBlocks as block, i (i)}
          {#if block.kind === "text"}
            <div class="text-ro">{block.text}</div>
          {:else}
            <pre class="tab-ro mono">{block.rows.map((r) => `|${r}|`).join("\n")}</pre>
          {/if}
        {/each}
      </div>
    {/if}

    {#if $openSong.orphan_notes.length > 0}
      <button class="orphan-toggle mono" onclick={() => (showOrphans = !showOrphans)}>
        {$openSong.orphan_notes.length} notes from removed sections {showOrphans ? "▾" : "▸"}
      </button>
      {#if showOrphans}
        <ul class="orphans">
          {#each $openSong.orphan_notes as o (o.label)}
            <li>
              <span class="olabel mono">{o.label}</span>
              <Button variant="chip" onclick={() => void actions.setSectionNotes(o.label, { blocks: [] })}>clear</Button>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  </Box>
{/if}

<style>
  .doc {
    display: flex;
    flex-direction: column;
    gap: 8px;
    /* Grow to fit the notes/tabs; the stage scrolls when the flow gets long
       (no internal scroll window that would cap the box short of its content). */
  }
  .text-row {
    display: flex;
    align-items: flex-start;
    gap: 4px;
  }
  .text {
    flex: 1 1 auto;
    min-height: 44px;
    resize: vertical;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 4px;
    color: var(--fg);
    padding: 6px 8px;
    font-size: 12px;
    line-height: 1.5;
    white-space: pre;
  }
  .text-row .del {
    flex: 0 0 auto;
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    line-height: 1;
    padding: 2px;
  }
  .text-row .del:hover { color: var(--fg); }

  .inserter {
    display: flex;
    gap: 8px;
  }
  .inserter button {
    background: none;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    color: var(--muted);
    cursor: pointer;
    font-size: 11px;
    padding: 2px 8px;
  }
  .inserter button:hover {
    color: var(--fg);
    border-color: var(--muted);
  }

  /* display mode: tight, read-only */
  .display {
    gap: 4px;
  }
  .text-ro {
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg);
    white-space: pre-wrap;
  }
  .tab-ro {
    margin: 2px 0;
    font-size: 12px;
    line-height: 20px;
    color: var(--fg);
  }
  .orphan-toggle {
    margin-top: 8px;
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    font-size: 11px;
    padding: 0;
    text-align: left;
  }
  .orphan-toggle:hover { color: var(--fg); }
  .orphans {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .orphans li {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .olabel {
    font-size: 11px;
    color: var(--muted);
  }
</style>
