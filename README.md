# video-wall (Rust rewrite)

Plays many local video files at once in a grid. Implements `RUST_REWRITE_SPEC.md`.

## Build and run

```
cargo build --release --locked
target/release/video-wall                 # lobby
target/release/video-wall --dirs ~/Movies --grid 3x2   # straight to the wall
cargo test                                 # unit + HTTP integration (ffmpeg required)
```

Requires `ffmpeg`/`ffprobe` (PATH, `FFMPEG_PATH`/`FFPROBE_PATH`, or the usual
platform directories) for `.mkv/.avi/.flv/.wmv/.3gp` remux and `.mpg/.mpeg`
transcode. Without them every file is played directly.

### Platform requirements

| OS | Build | Runtime |
|---|---|---|
| macOS 11+ | Xcode Command Line Tools | none (WKWebView is part of the OS) |
| Windows 10/11 | MSVC toolchain | WebView2 runtime (preinstalled on Win 11) |
| Linux | `libwebkit2gtk-4.1-dev libgtk-3-dev pkg-config` | `libwebkit2gtk-4.1-0`, `libgtk-3-0`, and `xdg-desktop-portal` for the folder/delete dialogs |

Linux build deps on Debian/Ubuntu:

```
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev pkg-config
```

## Releases

`.github/workflows/release.yml` builds on every `v*` tag (or manually via
workflow_dispatch) and publishes:

- `video-wall-macos-universal` — arm64 + x86_64 via `lipo`
- `video-wall-linux-x86_64`
- `video-wall-windows-x86_64.exe`

Each runner compiles natively, so the Linux binary links the runner's glibc.

## Shape

- `src/main.rs` — CLI, config merge, tao/wry window, native dialogs, event loop.
- `src/config.rs`, `src/cli.rs`, `src/log.rs`, `src/scan.rs`, `src/select.rs` —
  pure logic, unit-tested.
- `src/media.rs` — binary discovery, mode decision, ffmpeg args, ffprobe
  metadata formatting, the two-slot transcode queue.
- `src/server.rs` — 16 loopback media servers (token, Range, remux/transcode)
  and the app server (assets + JSON API).
- `src/assets/` — embedded renderer (lobby, wall, overlays, fullscreen).

## Deliberate decisions / deviations

- **Window startup**: opens at `min(90% of the screen, 1280x800 points)`,
  centred, and takes focus instead of the spec's 90% full-bleed window.
- **Product renamed `video-wall`**: binary, window/page title, toolbar, CLI usage
  and app-data dirs (`video-wall/config.json`, `~/.video-wall/logs/app.log`).
  The spec's `toomanyvids` paths existed so Electron settings would carry over;
  if that compatibility is wanted again, change the three strings in
  `config::user_data_dir` and `log::log_dir` back.

- **Rendering route 2 from §17**: system webview (wry) plus the HTTP transport,
  because codec support differs per OS only where the spec already expects
  remux/transcode fallbacks.
- **IPC → loopback JSON API**: the renderer talks to `/api/*`. Every call needs
  the per-run `X-Tmv-Token` that is embedded in the served page, so other
  senders are rejected (mirrors the Electron sender check). Media URLs carry
  the separate media token; both servers bind `127.0.0.1` only.
- **Strict uniqueness (§8.3)**: Next/Pick/auto-next never load a file that is
  already in another cell. When the wall is full, the cell keeps its current
  video (or the last frame) instead of duplicating. Pick then lists nothing.
- **CORS `*` on media** instead of `null`: the renderer origin is
  `http://127.0.0.1:<app port>`, not `file://`. It also lets the black-frame
  canvas read pixels (`crossorigin="anonymous"`).
- **Delete dialog** uses a native warning dialog with `Delete`/`Cancel`. On
  macOS the first (Delete) button is the default; the spec asked for Cancel.
- **`--headless`** is an extra testing flag: servers only, prints
  `URL http://127.0.0.1:<port>/`, no window.
- **macOS activate** does not recreate the window: the app quits when the last
  window closes, per §18.
- Reshuffle reuses the current pool (deleted files stay gone) rather than
  rescanning.

## Verification

`cargo test` covers config precedence/clamping/dedupe, scanner filters and
symlink loops, selection uniqueness, mode/ffmpeg args, metadata formatting,
range parsing, token shape, plus an end-to-end HTTP test that generates real
videos, starts a 4×4 wall, checks 200/206/416/403/404, valid remux and
transcode output, seek, 16 concurrent streams, config persistence, and CLI
auto-start. The renderer was exercised in Chromium against a headless instance
(lobby, wall, pause/mute, Next/Pick, seek, rotation, fullscreen, metadata,
HUD), and the native window was launched to confirm the webview auto-starts
the wall.
# video-wall
