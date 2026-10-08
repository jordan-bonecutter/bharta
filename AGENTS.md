# Working on bharta

## Build portability

`./build.sh` is the fresh-clone build entry point. It reports native prerequisites,
installs them together on supported distro families, provisions a recent Rust
compiler when needed, and builds missing GTK4 layer-shell privately from a pinned,
checksum-verified source release. Do not add undocumented native dependencies.
Preserve GTK4 4.6 compatibility and Rust 1.92 minimum unless an actual feature
requires a deliberate baseline change. The clean-image CI builds Ubuntu 22.04,
Ubuntu 24.04, and Fedora. Validate build-helper changes in disposable containers;
never install test dependencies into the user's desktop just to simulate a distro.

## UI testing

Use the isolated headless Sway harness for UI tests:

```sh
cargo build --release --examples
cargo build --release
python3 tests/headless_ui.py /tmp/bharta-headless
```

The harness creates a private Wayland compositor and D-Bus session, simulates
audio, and saves screenshots and logs. It requires Sway, grim, dbus-daemon, and
Python with Pillow. Run it with socket permissions if the sandbox blocks local
IPC. Inspect the screenshots as well as the assertions.

Do not run `ui_probe`, virtual input, clicks, drags, or keyboard injection on the
user's real desktop. The user explicitly finds mouse takeover disruptive. Keep
input tests on the harness's private Wayland socket and HEADLESS-* outputs. Never
fall back to the user's session if the headless setup fails. Do not change their
real audio levels for tests.

Run `cargo test` and `cargo clippy --all-targets -- -D warnings` for relevant code
changes. Socket-based unit tests need local IPC access.

## UI direction

Preserve the original compact, macOS-inspired bar: 28px height, centered app
name, understated neutral palette, small monochrome icons, and flat menu rows.
GTK is the widget/interaction framework, not a request for GNOME/Adwaita styling.
Avoid orange accents, large pill buttons, bulky headings, and instructional text
on obvious controls. Prefer borderless popups; avoid nested frames. Native slider
dragging must work continuously. Preserve the speaker control position across
play/pause changes and bound workspace previews independently of captured image
dimensions. Per-app equalizer bars must monitor their own audio stream, never
the mixed output or fabricated animation; stop stream capture when the drawer closes.
For headless meter tests, give each fake `parec --monitor-stream=ID` a distinct
tone and verify it appears only in that source row's matching frequency band.
Use two MPRIS fixtures with distinct artwork to verify all source images load.
Keep image, meter, mute, and playback control columns aligned across source rows;
ellipsize long summaries and omit generic ALSA/AudioStream backend names.
Keep source order independent of playback state and track titles. Paused sources
retain their slider and row height; metadata changes must not rebuild all rows.
All menus open on a short hover; leaving the bar and popup starts a 500ms delay
followed by a brief fade. Keep Wi-Fi names and actions left-aligned. Only one
popup may be open across all outputs at a time. Hovering another button must
switch directly to its menu, including workspace previews and clicked menus.
Apps focuses its search field on opening, including hover, so typing works immediately.
Workspace audio activity should be visibly marked with a speaker icon and accent.
Fade and scale the speaker within its reserved slot; playback must not resize
the workspace button or shift the surrounding controls.
The Sound drawer should show individual PulseAudio-compatible app streams with
per-stream volume and mute controls, using app/media names when available.

Commit completed changes as requested by the user. No confirmation is needed
for ordinary code edits.
