# Changelog

All notable user-facing changes to dredge, newest first. Entries are written
at release time by `scripts/ship release`.

## v0.15.0 — 2026-07-29

### New

- **Clicks can snap to the grid.** A new "single click snaps" setting (settings → editing): with grid snap on, clicking the waveform moves the playhead to the nearest bar, beat, or section edge instead of exactly where you clicked. Off by default — leave it off to keep free clicking.

### Improved

- **The notes box grows with your notes.** It no longer stops at a fixed height with its own scrollbar; long notes and tablature flow down the page.
- **Number inputs are wider,** so larger values no longer get cut off.

### Fixed

- **Pausing while muted now really pauses.** Before, pausing with the sound muted kept the song advancing silently, so unmuting picked up somewhere further along.

## v0.14.2 — 2026-07-17

### Fixed

- **Downloads now build.** The v0.14.0 and v0.14.1 Linux builds failed on the build server and never published downloads. This release fixes the build — it includes everything listed under v0.14.0.

## v0.14.1 — 2026-07-17

### Fixed

- **Downloads now build.** v0.14.0's Linux build was missing a required audio library on the build server, so its downloads never published. This release fixes the build — it includes everything listed under v0.14.0.

## v0.14.0 — 2026-07-17

### New

- **Foot pedal control.** Map a MIDI foot pedal (or any MIDI controller) to dredge, so you can loop, jump, and switch mixes hands-free while you play. A new Pedal tab walks you through it: arm a slot, press the pedal, and it learns the button.
- **Markers.** Drop numbered markers on the waveform and click one to jump straight to that spot.
- **Isolation snapshots.** Save a listening mix — which stems and bass-focus you're hearing — as a numbered chip, then recall it in one click. Right-click a chip to clear it.
- **Per-song isolation memory.** dredge now remembers how you set each song's stem mix and bass focus, and restores it when you reopen the song.

### Improved

- **Stop analyzing.** You can now stop structure analysis or stem separation after it starts — a Stop button on the ANALYZING panel aborts the run and frees your GPU. Anything that already finished is kept. A failed run also gets a Close button now.
- **Library sorting.** The song list is sorted by band, then song title, alphabetically (ignoring capitalization).
- **Recording takes.** Recorded takes show a real waveform, and you get a live input-level meter while arming a recording.

### Fixed

- **Dock tabs.** The tab bar is one continuous target now — no dead gaps between tabs when clicking or dragging.
- **Waveform zoom at launch.** The waveform reliably fits the window on startup instead of occasionally opening mis-zoomed.

## v0.13.0 — 2026-07-11

### New

- **Six stems instead of four.** Separation now uses the six-channel demucs model, so guitar and piano get their own faders alongside vocals, drums, bass, and other — each with mute and solo. The big win is for piano-heavy songs: with piano separated out, the bass stem stops swallowing the piano's left hand. Songs separated before this release show a "Separate stems" button — run it once per song to get the new stems.
- **Redo a separation.** The "separate stems" button in settings now redoes the open song's separation from scratch: the old stems are cleared and demucs runs fresh. Useful when a separation came out bad or after a model upgrade like this one.

### Improved

- **The analyzing readout gets room to breathe.** While analysis or separation runs, the isolation box takes a full row, so the progress steps and resource meters render at a comfortable width instead of squeezing into a column. A re-separation is also visible now — previously a song that already had stems showed no progress at all.

### Fixed

- **Clicks landing in the wrong place.** On some systems the app's click targets could drift out of line with what's on screen (for example, only the top half of a tab responding). The app now forces its display and input scales back into sync whenever the window resizes, regains focus, or becomes visible again.

## v0.12.0 — 2026-07-11

### New

- **Rerun stem separation when it didn't happen.** A song that's analyzed but has no stems (a failed run, a bundle copied without them) now shows a "Separate stems" button right in the isolation box, and there's another one in settings under analysis. Previously that state was a dead end.
- **The analyzing meters now show the whole run, not just the moment.** cpu, ram, gpu, and vram each draw their full history as a trace zoomed to the run's own range, so you can see the shape of the work — with a thin gauge on the right edge showing how close to the limit it got (it turns amber when busy, red near the ceiling). Meters are paired by what moves together: cpu with ram, gpu with vram.

### Fixed

- **Stem separation works when dredge is launched from the desktop.** dredge now finds demucs installed with `uv tool install` even when the desktop session's PATH doesn't include `~/.local/bin` — previously it claimed demucs wasn't installed and refused to run.
- **The analyzing readout fits its box.** Error messages wrap as readable text instead of one word per line, and the resource meters no longer overlap their labels and numbers in a narrow box.

## v0.11.0 — 2026-06-28

### Improved

- **Hide control boxes you don't use.** Hover a box's header and click the × to take it off the stage; a **+** in the stage's bottom-right corner pops up a list of the hidden tools to bring any of them back. The boxes you keep stay where you put them.
- **Collapse a box by tapping its header.** Tap a box's title bar to fold it down to just the strip, tap again to open it back up — no more aiming for a little caret. (The caret is gone.)
- **See what you're dragging.** Reordering a box now shows its name trailing your cursor and a line marking exactly where it will drop, so it's clear what's moving and where it'll land.
- **Tidied the control boxes.** "click" is now a compact vertical panel; an empty notes section shows just its header instead of a blank panel; the recordings span and input are compact dropdowns instead of full-width bars; and bass focus is a single toggle with the stem reset tucked beside it.
- **The tuner powers itself.** It listens whenever it's expanded and stops when you collapse it — no power button. It starts collapsed, so it stays silent until you open it to tune.

## v0.10.0 — 2026-06-27

Packaging and install only this release — no app changes (the reorderable/collapsible control boxes shipped in 0.9.0).

### Packaging

- **Arch: install with `yay -S dredge`.** The prebuilt `dredge-looper-bin` AUR package has been removed — it was built on Ubuntu and couldn't start on Arch, because Arch ships a newer rubberband than the prebuilt binary was linked against. The `dredge` package builds from source against your own system libraries and is the supported Arch path; if you had `dredge-looper-bin`, `yay -S dredge` replaces it.
- **Fixed building `dredge` from the AUR on machines with an older Node.** The build now pins its own pnpm version, so it no longer fails when your system's pnpm is newer than its Node.
- **Debian / Ubuntu is unchanged** — keep installing the `.deb` from the releases page.

## v0.9.0 — 2026-06-27

### Improved

- **Tidy the control boxes under the transport.** Each box — metronome, isolation, notes, recordings, tuner, and the rest — now collapses to just its title bar: click the caret to fold one away when you're not using it, click again to bring it back. Less scrolling when the stage gets busy.
- **Reorder the boxes to taste.** Drag a box by its header to move it; the order you set sticks between sessions. The waveform and play controls stay put at the top.

## v0.8.0 — 2026-06-27

### New

- **Practice routines.** Build a routine out of blocks — each block is a loop with its own speed, mix, and count-in — then launch it and let dredge advance from one block to the next automatically as each loop comes back around. Drag blocks to reorder them, or click a block's name to jump straight to it. The block you're on is highlighted on the waveform.
- **Record yourself over the track.** Arm recording in the recordings box and trigger or stop it from the transport. Your takes stack as layers beneath the waveform and play back locked to the song's timeline, so what you played lines up with where you played it. Choose which input to record from and whether to capture starting at the playhead.
- **Latency calibration.** A calibration screen measures your system's round-trip audio delay — and shows the round trip so you can see it — so recorded takes land in time on their own. The Devices panel shows which recording latency is in use (auto-detected or calibrated).
- **Dock panels on both sides.** The left and right columns are now docks. Drag a tab (structure, loops, routines, export, library, and the rest) to reorder it, stack tabs into a panel, split one off into its own panel, and drag a tab from one side to the other. Resize stacked panels by dragging the divider between them. The library is now just another tab — put it wherever you like.

### Improved

- **Bar-by-bar navigation.** The Left and Right arrow keys step the playhead a bar at a time and speed up as you hold them. Touching a fader no longer steals the arrow keys mid-practice.
- **Click to play, or click to place.** A toggle decides whether clicking the waveform starts playback from that spot or just moves the playhead there.
- **One collapse handle per side.** Each column hides and shows from a single full-height edge rail — slam the pointer to the window edge and it's there.

### Fixed

- **The playhead now tracks what you're hearing**, not where the engine is reading ahead — so the cursor sits where the sound actually is.
- **Overlapping loops.** Hovering a selection that sits on top of a saved loop no longer pops the loop's controls out from under it.
- **Switching songs no longer bleeds audio** from the previous song's recorded layers.
- **Fixed a UI freeze** — a render loop that could lock up the interface.

## v0.7.0 — 2026-06-24

### New

- **Count-in.** A pre-roll click counts you in before playback starts, so you're not scrambling on beat one. Set how many beats to count (1–8), and choose whether it counts once at the start or before every loop pass. The playhead holds and pulses through the count so you can see the beat land. (Needs an analyzed song, so it knows the tempo and time signature — it even defaults the beat count to the song's meter.)

- **Click track with per-section guides.** Mark any section to get a beat click while it plays. This is built for drilling along to the isolated drums: when a section drops the drums out, the click fills the gap so you hold time and land back in the pocket when the band returns. It rides the song's real beat grid — locked to the actual timing even as you slow things down — and stops cleanly at the section boundary. Count-in and the section click now share one **Click track** control.

- **Metronome.** A standalone practice metronome that works with or without a song loaded — open dredge and drill scales to a click, no track required. Set the tempo or tap it in, pick a time signature, and choose how often it clicks (every beat, every half-bar, or just the downbeat) and what it sounds like: a plain click, a kick/snare, or a cowbell. A row of lights shows your place in the bar. With a song open, one tap borrows its tempo. The kick/snare plays a real groove — a backbeat in 4/4 (kick on 1 and 3, snare on 2 and 4), a waltz feel in 3/4 — and odd meters like 5/4 and 7/8 accent the right beats.

### Fixed

- **The playhead stays inside your loop.** While playing a loop, the playhead no longer drifts outside the loop box — it stays clamped to the section you're drilling.

- **Editing your song structure keeps your click guides.** Re-drawing sections no longer silently clears which ones have a beat-click guide, and the click updates immediately instead of clicking the old spans.

- **Section notes hold steady during the count-in.** The notes panel no longer flips to the next section while the pre-roll is counting you in.

## v0.6.0 — 2026-06-23

### New

- **Choose your audio output and input devices.** A new **devices** tab in the right panel lists your outputs and inputs. Pick one and playback moves to it immediately — even mid-loop — and your choice is remembered between sessions. **System default** follows whatever your system is set to, and a **reset to system** button puts both back.

### Improved

- **The tuner follows the input you picked.** By default the tuner listens to the input device chosen in the devices tab; you can still override it to a specific device just for the tuner.

## v0.5.1 — 2026-06-23

- **AUR prebuilt renamed to `dredge-looper-bin`.** The `dredge-bin` name on the AUR belongs to an unrelated package; install the prebuilt with `yay -S dredge-looper-bin`, or `yay -S dredge` to build from source. The package and binary are still `dredge`.
- **Branding.** The app is presented as "Dredge Looper"; the command and package name remain `dredge`.

## v0.5.0 — 2026-06-20

### Improved

- **Renaming a song renames its folder.** Renaming a song's title or artist now renames its bundle folder on disk to match, so your library folders stay in sync with what the app shows. The rename is refused while stem separation or analysis is running for that song, and a playing song reloads only when its folder actually moves.

## v0.4.0 — 2026-06-17

### New

- **Per-section notes, with tablature.** Every song section now has its own notes — free text plus inline tab you type into and resize by dragging its edges. A clean display mode for reading while you play, an edit mode for changing things. Notes autosave and stay attached to the section by name (your "verse 2" notes follow verse 2), and typing in them never triggers playback or other shortcuts.

### Improved

- **Feels like a real desktop app.** The web-style right-click menu (Back, Reload, Inspect…) no longer pops up anywhere in the app.

### Fixed

- **Accent color changes apply instantly on the waveform.** Picking a new theme color now recolors the loop, section, and selection markings right away, instead of only after you next touch the waveform.
- **Controls stay clickable in fullscreen.** Going fullscreen could make some controls — like the right-hand tabs — unreliable to click; they're solid again.

## v0.3.0 — 2026-06-17

### Improved

- **Bass focus now lives with the stems, in one "Isolation" box.** They do the same job — making one part of the song stand out — so they're together now instead of split across the screen. Bass focus is still a single click and works on any song right away, even before you separate it into stems.

### Fixed

- **Resizing a loop no longer cuts the sound.** Dragging a loop's edge while it played used to drop the audio for about a second; now it stays smooth as long as the playhead is still inside the loop.
- **The loop button on a selection stays clickable** when a selection sits on top of an existing loop.

## v0.2.2 — 2026-06-16

### New

- **`dredge-doctor`** — a terminal command that lists the optional tools (ffmpeg, analysis, stem separation), shows which are installed, and prints the command to add anything that's missing. The desktop app shows the same under Settings → capabilities.

### Improved

- **MP3 export works out of the box** — the package now pulls in `ffmpeg` by default, so exporting to MP3 (and separating stems) no longer needs a manual install.

### Fixed

- **Export file names** — the file-name field now shows the extension dredge adds (`.wav` / `.mp3`), and typing one yourself no longer doubles it (no more `track.mp3.mp3`).

## v0.2.1 — 2026-06-16

### Fixed

- **Release packaging** — fixes the Linux release build so this version publishes correctly. This is the first downloadable build carrying the v0.2.0 changes (export, opening video files, the Settings capabilities panel, the tidier loop toolbar, and the export `~`-path fix — see v0.2.0 below). No app behavior changed since v0.2.0.

## v0.2.0 — 2026-06-16

### New

- **Export your practice** — render a loop or the whole song to WAV or MP3 at the tempo and pitch you've been drilling at, with your stem mix and bass-focus baked in. There's a new "export" tab in the right-hand panel.
- **Open video files** — load an mp4 or mov and dredge pulls the audio track straight out of it.
- **See what's installed** — Settings now has a capabilities panel showing whether stem separation, structure analysis, and MP3 export are ready on your machine, with a full-or-partial summary at a glance.

### Improved

- **Cleaner loop toolbar** — a clearer "save loop" icon, and the grid/snap controls now tuck into a corner button that slides open when you want them.
- **Export shows progress and can be cancelled** part-way through a render.
- **The guide explains the side panels** — click the corner icons (or press Ctrl+[ / Ctrl+]) to hide and show the library and panels.
- **More audio formats play** — a wider range of files decode via an ffmpeg fallback.

### Fixed

- **Export to a `~` path works** — typing `~/Music` now lands in your home folder instead of erroring or creating a stray folder; a relative path is rejected with a clear message.
- **Export checks the folder and file name up front**, so a bad path fails instantly instead of half-way through a render.
- **Some mp4/mov files decode correctly now** — dredge reads the audio track instead of the container's default track.

### Removed

- **System-audio capture and grab-back** have been removed.
