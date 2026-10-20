# TooManyVids — Functional Specification (Rust Rewrite)

Source of truth: the Electron implementation in this repository at version 0.1.3.
Purpose: let another agent rebuild the app in Rust with **the same user-visible behaviour**.
Everything marked "MUST" is observable behaviour; internal choices (HTTP vs. native decoding) may differ only where this document says so.

---

## 1. Product summary

TooManyVids is a desktop app that plays many local video files at the same time in one window, arranged in a grid. The user picks folders, a grid size and playback options on a setup screen, presses **Start Wall**, and all cells start playing simultaneously (muted by default). Initial selection and reshuffle load each video in at most one cell; the one exception is the manual Next/Pick fallback on a completely full wall (§8.3).

Two views in one window:

1. **Lobby / Setup screen** — folders, grid, shuffle/loop/mute, Start Wall.
2. **Wall** — full-bleed video grid with a 36 px toolbar that auto-hides.

Supported OS: macOS, Windows, Linux. Single window, native frame, title `TooManyVids - Multi-Video Player`, black background.

Non-goals (do not invent): playlists, searching, tagging, subtitles, audio routing, remote control, accounts, telemetry.

---

## 2. Glossary

| Term | Meaning |
|---|---|
| Pool | All video files found in the configured folders (unique). |
| Cell / tile | One grid slot. `rows * cols` cells exist. |
| Selected videos | Up to `rows * cols` distinct files from the pool, assigned to cells 0..n-1. |
| Wall | The playing grid view. |
| Lobby | The setup view. |
| Mode | How a file reaches the player: `direct`, `remux`, `transcode`. |

---

## 3. Configuration model

```jsonc
{
  "dirs": ["/abs/or/relative/path"],   // at least 1 to start a wall
  "grid": { "rows": 2, "cols": 2 },     // each 1..8, default 2x2
  "shuffle": true,                      // default true
  "loop": false,                        // default false
  "mute": true                          // default true
}
```

Rules:

- Defaults: `dirs: []`, `grid: {rows:2, cols:2}`, `shuffle: true`, `loop: false`, `mute: true`.
- Precedence when starting the app: **defaults < saved config < CLI arguments**.
  Only keys actually present in the CLI override saved values.
- Grid rows/cols are clamped to 1..8. Non-numeric → default 2.
- `dirs` are trimmed, empty/non-string values dropped, duplicates removed (preserve first occurrence).
- The config is saved only after a wall start succeeds (scan returned ≥ 1 video). Failed attempts do not overwrite saved settings.
- Saved location: Electron `app.getPath('userData')/config.json`
  - macOS: `~/Library/Application Support/toomanyvids/config.json`
  - Windows: `%APPDATA%\toomanyvids\config.json`
  - Linux: `~/.config/toomanyvids/config.json`
- Corrupt/unreadable config → treated as absent, defaults are used (log a warning, never crash).

---

## 4. CLI

All flags optional. Invocation: `toomanyvids [options]`.

| Flag | Effect |
|---|---|
| `--dirs <dir1> [dir2...]` | Folders to scan. Passing it **skips the lobby** and starts the wall immediately. Requires ≥ 1 value. |
| `--grid <ROWSxCOLS>` | e.g. `3x2`. Case-insensitive `x`. Invalid format → error message + exit. |
| `--shuffle` / `--no-shuffle` | Override saved shuffle. |
| `--loop` | Loop each video. |
| `--mute` / `--no-mute` | Start muted / with sound. |
| `--dev` | Open DevTools on start (desktop webview only; in a Rust build: dev mode / inspector). |
| `--help`, `-h` | Print usage (see below), exit 0. |

- Arguments before the first `--`-flag are ignored (Electron bootstrap paths).
- On CLI parse error: print `Error: <message>` plus a usage line to stderr, exit non-zero.
- Auto-start (when `--dirs` is given) uses the merged config and behaves exactly like pressing Start Wall; a failure shows the error modal and falls back to the lobby.
- Help text must list: usage, options, "settings are overridden by CLI", examples, keybindings, supported formats.

---

## 5. Startup sequence

1. Resolve `ffmpeg` / `ffprobe` (see §9.5).
2. Parse CLI, read saved config, merge.
3. Start the media transport (16 loopback HTTP servers in the Electron build).
4. Create one window: 90 % of the primary display's width/height, black background.
5. Renderer loads the lobby (or, with CLI dirs, immediately starts the wall).
6. Scanning happens **only** when a wall starts — never at app launch.
7. Closing the last window quits the app (all platforms) and stops the servers. On macOS, `activate` recreates the window if none exists.

No single-instance lock exists; multiple instances are allowed (each independent).

---

## 6. Lobby (setup screen)

Full-window, dark background, centered card (560 px wide, max 100 %, scrollable). Contents top to bottom:

### 6.1 Folders

- List of selected folder paths, each row: path (ellipsized, full path on hover tooltip), **video count**, remove button (`×`).
- Empty state: `No folders yet`.
- `+ Add folder` opens the native multi-select folder picker (`openDirectory` + `multiSelections`).
- Each added folder triggers a recursive scan; the count appears as:
  - `…` while scanning,
  - `N videos` (or `1 video`) when done,
  - `not found` in red if the path is missing/inaccessible/not a directory.
- Counts are refreshed for saved folders on lobby load, for newly added folders on add. Removing a folder needs no scan.
- Counting uses the same rules as the wall scan (§8.1), so the number equals what would be playable from that folder.

### 6.2 Grid

- Preset buttons: `2×2`, `3×3`, `4×2`, `4×4`. Active preset (matching current values) is highlighted.
- Two number inputs `rows` and `cols`, min 1, max 8, default 2/2.
- Live label `N videos` = `rows * cols`.
- Changing inputs updates the preset highlight.

### 6.3 Playback

Three checkboxes: **Shuffle** (default on), **Loop** (default off), **Mute** (default on).

### 6.4 Footer

- Inline status/error text (red, left), **Start Wall** button (primary, right).
- While starting: button disabled, label `Starting...`. Afterwards: `Start Wall` again.
- Errors: `Add at least one folder` (client side), `No video files found in the selected folders`, `Failed to scan folders: <err>`, `Failed to start: <err>`.
- Hint line: `Press Tab, Esc or the Setup button while playing to come back here. Settings are saved on start.`

### 6.5 Start Wall

1. Validate ≥ 1 folder.
2. Send `{dirs, grid, shuffle, loop, mute}` to the backend.
3. Backend scans, persists settings, builds selection and sends the wall config.
4. On success, the lobby hides and the wall appears.
5. On error, the lobby stays visible with the error text.

---

## 7. Wall

### 7.1 Layout

- `#app` is a vertical flex column: toolbar (36 px) + grid.
- Grid: CSS grid with `rows`/`cols`, gap 2 px, 2 px padding, background `#111`.
- Cells: `background: #0a0a0a`, radius 4 px, `overflow: hidden`; video uses `object-fit: contain`, black background.
- Empty cells (when the pool has fewer videos than cells) are plain black, no spinner, no text.

### 7.2 Toolbar

Left to right: title `TooManyVids`, status text, then **Setup**, **Reshuffle**, **Pause**/**Resume**, **Unmute**/**Mute**.

- Status: `<rows>x<cols> | <n> playing` (n = cells whose video is not paused). Updated on video `loadeddata`, window resize, pause toggle.
- Button tooltips: `Back to setup (Esc or Tab)`, `Reshuffle (R)`, `Pause/Resume (P)`, `Toggle Mute (M)`.
- Mute label reflects state: muted → `Unmute`, unmuted → `Mute`. Pause: playing → `Pause`, paused → `Resume`.
- **HUD auto-hide:** after 2 s without mouse/keyboard input the toolbar slides up/out (`translateY(-100%)`, opacity 0, 250 ms ease) and the cursor is hidden. Any mouse move/click/wheel/keydown brings it back and restarts the 2 s timer. Never hides in the lobby.

### 7.3 Cell overlay (per tile)

Hidden by default, fades in on pointer hover over the cell (`opacity 0 → 1`, 200 ms), hidden again on leave (also closes the pick list).

Contents bottom-up:

1. Seek row: range slider 0–100 + time text `m:ss / m:ss`.
2. Buttons row:
   - `⏩ Next` — load a random available video.
   - `☰ Pick` — toggle a scrollable list (max height 120 px) of all available videos by filename without extension; click loads it.
   - `Meta` — open metadata dialog (§7.5).
   - `Del` — delete from disk after confirmation (§7.6).
   - Rotate group: `↶` (−90°) and `↷` (+90°).
3. Title (filename without extension) and info (human-readable size) with gradient scrim.

Overlay must not steal clicks from the video except on its own controls.

### 7.4 Cell interactions

- **Click** (single): toggle play/pause of that cell. A double-click must not toggle twice — single click is delayed 250 ms and cancelled when a second click arrives.
- **Double-click:** enter fullscreen for that cell; double-click in fullscreen exits. Empty cells ignore double-click.
- **Hover:** show overlay; leaving hides it and closes the pick list.
- **Ended:** if `loop` is on, the video loops natively. Otherwise load a random available video into the cell.
- **Seek:**
  - `direct` mode: set `video.currentTime`.
  - `remux`/`transcode` (or when currently using the transcode fallback): reload the stream URL with `&t=<floor(seconds)>`; restore mute/loop; play. This is a coarse seek (server restarts ffmpeg at that timestamp).
  - While dragging the slider: update the time label only; apply on pointer up.
- **Rotation:** ±90° increments, normalised 0..270. When rotated by 90/270, scale the video so it still fits the cell: `scale = min(cellW / displayedH, cellH / displayedW)` where the displayed box is the contain-fit of the unrotated video. Re-applied on window resize and when entering/exiting fullscreen.
- **Delete:** see §7.6. If no videos remain, the cell shows `No videos left`; otherwise it loads a random available video.

### 7.5 Metadata dialog

Triggered by `Meta` in a cell, or a toast `No video loaded yet` if the cell has no video.

Modal overlay (darkened backdrop, centered card, max width 680 px). Title `Video metadata`. Monospace, selectable `<pre>` with one line per available field, in this order and format:

```
File: <absolute path>
Container: <format_long_name>
Duration: m:ss
Size: <human readable, e.g. 25.1 MB>
Bitrate: <round(bitrate/1000)> kbps
Video: <codec_long_name or codec> <width>x<height> <fps> <pix_fmt>
Video Profile: <profile>            (only if present)
Rotation: <degrees>°                (only if present)
Audio: <codec_long_name or codec> <sample_rate>Hz <channels>ch
```

Buttons: **Copy** (clipboard + toast `Metadata copied to clipboard`, or `Copy failed: <err>`) and **Close**. Backdrop click closes. `Esc` closes the dialog first (before fullscreen/lobby handling). Fields unknown/absent are omitted. Without ffprobe: toast `Meta error: ffprobe not available`.

### 7.6 Delete

- Native confirmation dialog: title/type warning, buttons `Cancel` (default, cancel) and `Delete`, message `Delete video from disk?`, detail `<path>\n\nThis cannot be undone.`
- On confirm: delete the file, remove it from the pool and path map, remove it from the current cell, load a random available video into that cell (or show `No videos left`).
- Toast `Deleted <filename>`; on failure `Delete failed: <err>`.
- The deleted file must never appear in Pick/Next again without a rescan.

### 7.7 Fullscreen

- Per-cell, not OS-fullscreen; implemented as an in-window overlay covering the app (z-index above toolbar).
- Reuses the cell's current stream URL and `currentTime`; pauses the cell video.
- Overlay: title, info, seek slider + time (same interactions; direct seek only), hint `Press ESC or double-click to exit`.
- On exit: copy the fullscreen `currentTime` back to the cell video and resume it.
- Rotation is preserved; resizing re-applies the rotation scale.
- `Esc` exits fullscreen first; if not fullscreen it returns to the lobby.

### 7.8 Errors and toasts

- In-cell messages (persist until clicked to retry):
  - `Cannot play: .EXT` (fallback also failed),
  - `Codec? : .EXT` for `mkv/avi/flv/wmv/hevc/h265`,
  - `Error: .EXT (code N)`,
  - `Click to play: <title>` when autoplay was blocked (click retries playback).
- Global error modal: centered card, title `Error`, message, `OK` button. Used for fatal/lobby-level failures.
- Toast: bottom-center, dark pill, 2.5 s, replaces the previous toast.

### 7.9 Keyboard

| Key | Action |
|---|---|
| `Tab` | Wall → lobby (pause all cells). In the lobby it is a normal focus key. |
| `Esc` | Close metadata dialog → else exit fullscreen → else wall → lobby. In the lobby: no action. |
| `R` | Reshuffle |
| `P` | Pause/resume all cells + update button |
| `M` | Toggle mute for all cells + update button |
| `?` | Toggle shortcut overlay |
| `Q` | Quit application |

Keys are ignored while the focus is in an `input`, `textarea` or `select` (lobby fields).
Shortcut overlay content: `Tab/Esc Back to setup | R Reshuffle | P Pause/Resume | M Mute/Unmute | ? Toggle Help | Q Quit`.

### 7.10 Reshuffle

- New random selection of up to `rows*cols` **distinct** videos, new pool included.
- Cells beyond the number of selected videos are cleared (video stopped, `src` removed, overlay/title/info emptied, no loading indicator).
- Autoplay/mute/loop state follows the current config; status text updates.

## 8. Media discovery and selection

### 8.1 Scanning

- Recursive directory walk. Symlink policy: follow only what the current Node scanner follows (it uses `readdir` + `stat` on entries; directories are recursed; no cycle detection — a Rust port SHOULD avoid infinite symlink loops).
- Accepted extensions (case-insensitive): `.mp4 .mkv .avi .mov .webm .flv .wmv .m4v .mpg .mpeg .3gp .ogv`.
- A file is playable only if: extension matches **and** filename does not start with `._` (macOS resource forks) **and** size ≥ 1 MiB (1 048 576 bytes).
- Missing or unreadable directories are skipped with a warning; scanning continues with the others.
- Results from multiple folders are concatenated and deduplicated by path (exact string equality of the resolved path).
- Scanning is sequential per folder. No caching between scans.
- Count shown in the lobby per folder uses exactly this filter.

### 8.2 Selection (the uniqueness invariant)

- Pool = all scanned files. Cells = `rows * cols`.
- Take the pool, optionally Fisher–Yates shuffle, then `slice(0, min(cells, pool.length))`.
- **A video MUST NOT appear in more than one cell.** If the pool is smaller than the grid, the remaining cells stay empty.
- `shuffle=false` uses the scan order.
- The pool sent to the UI always contains all videos (for Next/Pick).

### 8.3 Next / Pick availability

Available for cell *i* = pool minus (path of cell *i*) minus (paths currently loaded in any other cell). If that set is empty, the current build falls back to pool minus cell *i*'s own path. Pick items show the filename without extension; the full path is the tooltip.

**Known deviation:** in a completely full wall (loaded cells == distinct files in the pool), that fallback makes Next/Pick/auto-next-after-end able to load a file that is already playing in another cell. The initial selection and reshuffle never duplicate. A rewrite should decide explicitly: either preserve this fallback or keep strict uniqueness (leave the cell as-is / show nothing when no unused video exists).

---

## 9. Playback pipeline

The Electron app streams via loopback HTTP. A Rust port may decode natively (e.g. libmpv) **only if** it preserves the user-visible behaviour in §7 and the fallback semantics below. If a Rust port keeps a webview, it must keep the transport rules in §10.

### 9.1 Mode decision per file extension

| Mode | Extensions | Behaviour |
|---|---|---|
| `direct` | everything else (`mp4, mov, webm, m4v, ogv, …`) | Natively playable; streamed with HTTP Range. |
| `remux` | `.mkv .avi .flv .wmv .3gp` | ffmpeg stream-copy to fragmented MP4 (no re-encode). |
| `transcode` | `.mpg .mpeg` | ffmpeg re-encode to H.264/AAC. |

- Without ffmpeg available, every file is treated as `direct` (browser support decides).
- Every non-transcode file whose extension is in the fallback set gets an alternate `transcode` URL: `.mov .m4v .mkv .avi .flv .wmv .mpg .mpeg .3gp .ogv`.
- If direct playback fires `error`, or the rendered frame is black (see 9.4), the cell switches to the fallback URL. Switching happens at most once per load.
- User-visible consequence: containers/codecs that the platform cannot decode still play, at the cost of a short re-encode delay.

### 9.2 ffmpeg invocation (exact)

Remux:
```
ffmpeg [-ss <t> -noaccurate_seek] -i <file> -c copy -metadata:s:v rotate=0 \
       -f mp4 -movflags frag_keyframe+empty_moov -
```
Transcode:
```
ffmpeg [-ss <t> -noaccurate_seek] -i <file> -c:v libx264 -preset ultrafast -crf 28 \
       -c:a aac -b:a 128k -f mp4 -movflags frag_keyframe+empty_moov -
```
- Output goes to the HTTP response (`stdout.pipe(res)`), `Content-Type: video/mp4`, `Cache-Control: no-cache`.
- `t` comes from the seek parameter; `-ss` is inserted before `-i`.
- Killing the response (client closed/seeked away) MUST kill the ffmpeg process.
- Exit code 0 and 255 are not errors (255 occurs on early termination).
- At most **2 simultaneous transcodes**; additional transcode requests queue and start as slots free. Remux is not limited.
- ffprobe (`-v quiet -print_format json -show_format -show_streams`) provides metadata; fields used are those listed in §7.5.

### 9.3 Binary discovery

Look up `FFMPEG_PATH` / `FFPROBE_PATH` env vars first, then `ffmpeg`/`ffprobe` on `PATH`, then platform-specific locations:
- macOS: `/opt/homebrew/bin`, `/usr/local/bin`
- Windows: `C:\ffmpeg\bin`, `%LOCALAPPDATA%\Microsoft\WinGet\Links`

A candidate counts only if `<binary> -version` exits 0 within 3 s. ffmpeg/ffprobe are not bundled.

### 9.4 Black-frame detection (direct playback only)

- Only for cells that have a fallback URL and only while not already using fallback.
- Trigger 1.5 s after `playing`, or 0.5 s after a `timeupdate` with `currentTime > 0.5`.
- Draw the video into a 4×4 canvas; if all 16 sampled pixels have RGB ≤ 5 in every channel, retry up to 4 times, 1 s apart.
- After 4 black samples: switch to the transcode fallback (preserve mute/loop, show loading, play).
- Canvas read errors (CORS/taint) abort the check silently after the retry budget.

### 9.5 Playback states per cell

- Loading indicator `Loading...` visible until `loadeddata`/`canplay`, or while seeking, or while stalled.
- Autoplay uses `playsinline`, `autoplay`, `muted`; muted default comes from config. Autoplay blocked → `Click to play: <title>`; clicking the cell retries.
- `stalled`: show loading. Before the first frame this is expected (ffmpeg warm-up) → log once per load at DEBUG. After playback started, log a WARN at most once per cell per 30 s.
- `error`: log, then fallback if available and unused; otherwise show the in-cell error text from §7.8.
- Cell state resets on every load: URL/path/size/ext/mode/fallback flag, rotation, black-frame counters, stall counters.

### 9.6 Throttling / startup behaviour

- Cells load staggered by 250 ms per cell index (index 0 immediately, then 250 ms, 500 ms, …).
- Grid cell count is fixed at wall start; reshuffle keeps it.
- 16 simultaneous streams must be supported on all three OS.

---

## 10. Transport (Electron reference implementation)

If the Rust port keeps an embedded webview, reproduce this; otherwise it is informative.

### 10.1 Endpoint

`GET http://127.0.0.1:<port>/video?id=<id>&token=<token>[&mode=remux|transcode][&t=<seconds>]`

- Servers bind **only** to `127.0.0.1`, one random port per server. **16 servers** exist because Chromium allows 6 concurrent HTTP/1.1 connections per origin; long-lived ffmpeg pipes would otherwise starve cells past the 6th.
- Port for cell index `i` = `ports[i % 16]`.
- Token = 128-bit random hex per app run. Wrong/missing token → `403` before any id handling. Unknown id → `404`.
- `mode` is honoured only if ffmpeg is available; otherwise the extension decides.
- `t` = seek offset in seconds.

### 10.2 Direct responses

- `Accept-Ranges: bytes`, `Access-Control-Allow-Origin: null`, `Vary: Origin`, `Access-Control-Allow-Headers: Range`, `Access-Control-Expose-Headers: Content-Length, Content-Range`.
- No `Range`: `200` + full length + streamed file.
- With `Range: bytes=start-end`:
  - invalid start (`<0` or `>= size`) → `416` + `Content-Range: bytes */<size>`.
  - else `206` + `Content-Range: bytes start-end/size` + `Content-Length`.
  - `end` is capped at `start + 2 MiB − 1` (chunked streaming; the player re-requests the rest).
- Content-Type by extension: mp4/mov/m4v → `video/mp4`, mkv → `video/x-matroska`, avi → `video/x-msvideo`, webm → `video/webm`, flv → `video/x-flv`, wmv → `video/x-ms-wmv`, mpg/mpeg → `video/mpeg`, 3gp → `video/3gpp`, ogv → `video/ogg`, else `application/octet-stream`.
- File stream errors destroy the response.

### 10.3 Security requirements

- Renderer: `contextIsolation: true`, `nodeIntegration: false`, preload bridge only, no new windows, block in-app navigation.
- All IPC handlers reject senders that are not the app's renderer.
- CSP: `default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; media-src http://127.0.0.1:*; connect-src http://127.0.0.1:*; object-src 'none'; base-uri 'none'; form-action 'none'`.
- Media URLs carry the run token; CORS allows only origin `null` (the file:// renderer); no directory listing endpoint exists.
- The media server must not be reachable from other hosts (loopback binding only).

---

## 11. IPC surface (Electron reference)

Commands (renderer → main), all validated against the trusted sender:

| Name | Type | Payload | Response |
|---|---|---|---|
| `start-wall` | invoke | `{dirs, grid:{rows,cols}, shuffle, loop, mute}` | `{ok:true}` or `{error:string}` |
| `pick-directories` | invoke | – | `{dirs: string[]}` (empty when cancelled) |
| `scan-folders` | invoke | `string[]` (≤ 50, deduped) | `{counts:[{dir, count:number\|null, error?:string}]}` |
| `get-video-meta` | invoke | video id | metadata object or `{error}` |
| `delete-video` | invoke | video id | `{deleted:true,path}` / `{cancelled:true}` / `{error}` |
| `reshuffle` | send | – | triggers a new selection |
| `log` | send | `{level,message,meta}` | writes to the log |

Events (main → renderer):

| Name | Payload |
|---|---|
| `lobby` | `{dirs, grid, shuffle, loop, mute, ffmpegAvailable}` |
| `config` | `{grid, videos:[entry], pool:[entry], loop, mute, ffmpegAvailable}` |
| `reshuffle` | `{videos:[entry], pool:[entry], loop, mute}` |
| `error` | `string` |

Video entry: `{id, url, fallbackUrl|null, path, size, ext, mode}`.
Ids: `v0..vN` for the grid selection, `p0..pM` for the pool. The id→path map is per run and cleared on every wall start.

---

## 12. Persistence and logging

- Config file: pretty-printed JSON, written on every successful wall start. No other writes.
- Logging: `~/.toomanyvids/logs/app.log` (Windows: `%USERPROFILE%\.toomanyvids\logs`), append-only, ISO-8601 timestamps.
  - Line format: `[<iso>] [<LEVEL>] [<logger>] <message> [<json meta>]`
  - Levels: DEBUG, INFO, WARN, ERROR. ERROR also prints to stderr; WARN to stderr as warning.
  - Rotation: size-based at 10 MiB, keep `app.log` + `app.1.log` … `app.10.log` (oldest deleted).
  - Loggers used: `main`, `cli`, `scanner`, `config`, `renderer`.
- Logging failures are swallowed (file logging must never break playback).

## 13. Visual design

Dark theme only. System UI font stack: `-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif`.

| Element | Values |
|---|---|
| App background | `#000` |
| Toolbar | height 36 px; gradient `#2d2d2d → #1a1a1a`; bottom border `#444`; padding 0 12 px; gap 12 px |
| Toolbar buttons | bg `#3a3a3a`, border `#555`, text `#ddd`, radius 4 px, 12 px font, padding 4×12; hover bg `#4a4a4a` |
| Status text | 12 px, `#888` |
| Grid | background `#111`, gap 2 px, padding 2 px |
| Cell | background `#0a0a0a`, radius 4 px |
| Cell title | 12 px, 500 weight, `#fff`, single line ellipsis |
| Cell info | 10 px, `#888`, single line ellipsis |
| Overlay scrim | `linear-gradient(transparent, rgba(0,0,0,0.9))`, padding 30/10/10, fade 200 ms |
| Seek bar | height 4 px, track `rgba(255,255,255,0.3)`, thumb 12 px white circle |
| Lobby background | `#0b0b0b`; card `#171717`, border `#333`, radius 10 px, padding 24 px, width 560 px |
| Lobby accent | `#2f6feb` (active preset, Start Wall), hover `#3d7cf2` |
| Lobby error text | `#ff8a8a`, 12 px |
| Metadata modal card | `#1a1a1a`, border `#444`, radius 8 px, max width 680 px; `<pre>` bg `#111`, monospace 12 px |
| Error modal title | `#ff6b6b` |
| Toast | bg `rgba(40,40,40,0.95)`, white, 13 px, radius 6 px, bottom 50 px, 2.5 s |
| Fullscreen overlay | same scrim, 40/20/20 padding, fade 300 ms |

Empty states: lobby folder list `No folders yet`; empty grid cells render nothing.

---

## 14. State machines

### 14.1 App view

```
startup ──saved/CLI config + no --dirs──▶ LOBBY
startup ──--dirs given──▶ scan ──ok──▶ WALL
                              └──error──▶ LOBBY (+ error modal)
LOBBY ──Start Wall ok──▶ WALL
LOBBY ──Start Wall error──▶ LOBBY
WALL ──Tab / Esc / Setup──▶ LOBBY (all videos paused)
```

### 14.2 Cell

```
EMPTY ──load──▶ LOADING ──canplay/playing──▶ PLAYING ──pause-click──▶ PAUSED
  ▲                 │                            │
  │                 └──error/black-frame──▶ FALLBACK_LOADING ──▶ PLAYING
  │                                                         └──error──▶ ERROR_TEXT
  └──reshuffle with fewer videos / delete without replacement──┘
ENDED (loop off) ──▶ load next available (or stay on last frame if none)
```

Rules:
- Loading the same cell twice must cancel timers from the previous load (black-frame, stall bookkeeping, fallbacks).
- Deleting a video other than the cell's own must not change that cell.
- Pausing is global state as well: `P` toggles `isPaused` and pauses/resumes every non-empty cell; a new wall start resets it to playing.

---

## 15. Edge cases and invariants

- **Uniqueness:** no path appears twice among loaded cells at any time; reshuffle preserves it; empty cells remain empty.
- **Pool smaller than grid:** `cells = rows*cols`, loaded cells = pool size, remaining cells blank (no `Loading...`).
- **Pool empty / no videos:** the wall is never entered; the lobby shows `No video files found in the selected folders`.
- **Folder disappears:** count shows `not found`; starting fails with the scan error / no-videos message.
- **Relative folder paths** are stored as typed and resolved against the process working directory; a Rust port should document or normalise this.
- **Duplicate folders** are deduped for counting; `scanDirectories` also dedupes files by resolved path.
- **Delete during playback of the same file:** the cell must stop and never keep a URL to the removed file.
- **Deleting the last file:** other cells keep their videos; the affected cell shows `No videos left`; Next does nothing.
- **Seek in piped modes** restarts ffmpeg; short freezes/loading are expected.
- **Window resize:** rotation scale is recomputed; status text is refreshed.
- **HUD hidden:** keyboard shortcuts still work; mouse movement restores the toolbar.
- **Lobby return:** all cells pause; the wall is torn down on the next Start (new scan, new grid, new URLs).
- **Config is only written on successful starts** — a failed attempt must not clobber good settings.
- **No telemetry, no network access beyond loopback.**

---

## 16. Acceptance criteria (observable)

A Rust build is behaviourally complete when all of these pass:

1. Fresh start without arguments opens the lobby; no directory scanning happens before Start Wall.
2. Adding two folders shows a per-folder count (`N videos`) that equals the number of playable files; a bogus path shows `not found`.
3. Grid presets and 1..8 inputs stay in sync; the label shows `rows*cols` videos.
4. Start Wall with 7 videos and a 4×4 grid ⇒ 7 playing cells, 9 empty black cells, no duplicates, no spinners.
5. Reshuffle keeps the uniqueness rule and re-fills only as many cells as there are distinct videos.
6. Settings persist across app restarts (`config.json`); CLI flags override saved values for that run and `--dirs` skips the lobby.
7. `Tab`, `Esc`, and the Setup button return to the lobby and pause all videos; `Esc` never quits the app; `Q` quits.
8. The toolbar disappears after ~2 s of inactivity and returns on mouse movement.
9. `P` pauses/resumes all cells and flips the button label; `M` toggles mute and flips `Mute`/`Unmute`; the status shows `RxC | N playing`.
10. Hover shows the overlay; Next/Pick load a video that is not active in another cell; Pick lists available files by name.
11. Double-click enters fullscreen; `Esc`/double-click exits and resumes from the same position.
12. Rotation ±90° keeps the video inside the cell; resize keeps it fitted.
13. Meta opens the dialog with absolute path, container, duration, size, bitrate, video codec/resolution/fps/pix_fmt, optional profile/rotation, audio codec/rate/channels; Copy copies the exact text; `Esc` closes only the dialog.
14. Delete asks for confirmation, removes the file, the cell, and the pool entry; the file never reappears in Pick until rescanned.
15. `.mkv`/`.avi`/`.flv`/`.wmv`/`.3gp` play via remux; `.mpg`/`.mpeg` via transcode (when ffmpeg is present); unsupported direct files fall back to transcode.
16. At least 16 simultaneous streams play on macOS, Windows and Linux (with ffmpeg installed for the formats above), including 16 `.mkv` cells, with no cell stuck on a loading indicator.
17. No file access endpoint is reachable without the run token; media server listens on loopback only.
18. Logs live in `~/.toomanyvids/logs/app.log`, rotate at 10 MiB, keep 10 files.

---

## 17. Suggested Rust shape (non-binding)

This is guidance for the rewrite, not a requirement.

- **Workspace split:**
  - `core`: config merge/sanitise + persistence, scanner, selection, path/URL mapping, logging. Pure, unit-testable (mirror `test/config.test.js`, `test/selectVideos.test.js`).
  - `media`: ffmpeg/ffprobe discovery, probe parsing, remux/transcode queue, metadata formatting.
  - `ui`: window(s), grid, overlays, dialogs, keyboard.
- **Crates:** `serde`/`serde_json` (config), `clap` (CLI), `walkdir` (scan), `thiserror`, `tracing` + `tracing-appender` (logs), `directories`/`dirs` (userData), `tokio` (processes/streams), `rand` (shuffle/token).
- **Video rendering, two viable routes:**
  1. **Native decode (preferred for performance):** `libmpv` via the `libmpv2`/`mpv` crate, one render context, N playlists/streams; GUI with `slint` or `egui`. Removes the HTTP transport entirely; keep the mode logic only for user-visible fallback messaging. Re-implement the overlay UI natively; fullscreen and rotation become view transforms.
  2. **Keep a webview (closest parity, highest compatibility risk):** `tauri` + system webview. Codec support then differs per OS (WKWebView cannot play MKV; WebKitGTK depends on GStreamer). If this route is chosen, keep the HTTP transport and port sharding from §10, and expect remux/transcode to be used more often.
- **Grid layout:** fixed rows/cols, `object-fit: contain` equivalent, 2 px gutters; empty cells are intentional.
- **Timing:** 250 ms staggered starts, 2 s HUD timeout, 2.5 s toasts, 30 s stall-log throttle.
- **Persistence path:** use the OS app-data dir named `toomanyvids` so existing configs keep working.
- **Tests to port:** config precedence/clamping/dedupe; selectVideos uniqueness/order; a UI smoke that starts a 2×2 wall and asserts 4 distinct playing cells, plus a 4×4 wall with 7 files asserting 7 playing + 9 empty.

---

## 18. Known quirks of the Electron build (preserve or consciously change)

- `Esc` returns to the lobby; quitting is `Q` or closing the window.
- The pool URLs (`p*`) are generated for all files, but only selected ones (`v*`) are loaded on wall start.
- Grid changes require a new Start Wall (no live relayout).
- Fullscreen is an in-window overlay, not OS fullscreen.
- The app quits when the last window closes, including on macOS.
- Multiple app instances can run concurrently (no single-instance lock); each has its own ports/token.
- Media URLs are per-run and die with the process; there is no deep-link or file-open handling.
- Toolbar auto-hide is paused while the lobby is visible.
- `--dev` is Electron-specific; a Rust port can map it to a debug console or ignore it.
- Next/Pick/auto-next may duplicate a video only in the full-wall fallback described in §8.3; start-of-wall selection and reshuffle stay unique.
- CLI auto-start fires on the first renderer load only; reloading the window shows the lobby.
