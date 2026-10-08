# bharta

A small, macOS-inspired status bar for Sway, written in Rust. Native Wayland
layer-shell, software rendering, no GTK, webview, icon fonts, or status scripts.

![Light theme](preview-light.png)

## Run

Requires Linux, a current Rust toolchain, Sway, libxkbcommon, and a supported local font.
Wi-Fi uses your running iwd service directly over D-Bus, or NetworkManager through
`nmcli`. The native launcher uses `gio launch` to honor desktop-entry launch rules.

```sh
cargo run --release
cargo run --release -- --dark --all-outputs
cargo run --release -- --output DP-1
cargo run --release -- --font /path/to/font.ttf
```

The bar reserves 28 logical pixels at the top of each active output, without taking
keyboard focus except while a popup is open. By default (or with `--all-outputs`),
a Rust supervisor checks outputs every second, starts a bar for newly connected
displays, and restarts exited bars. `--output NAME` runs just one display.
Only one supervisor runs per Sway session; stopping it also stops its bars.

To use it instead of swaybar, remove/comment your existing `bar { ... }` block
and add this line to your Sway config after building with `cargo build --release`:

```sway
exec /home/jordan/Projects/bharta/target/release/bharta --dark --all-outputs
```

Adjust the path if you move the repository. Use `exec`, not `exec_always`, to
avoid duplicate instances on config reload. No Sway configuration is changed
by building or running this project. Ctrl-C stops a foreground instance.

## Current behavior

- Light and dark translucent surfaces with a subtle bottom separator.
- SF Pro Text when installed at `~/.local/share/fonts/SF-Pro-Text-Regular.otf`;
  DejaVu Sans and Liberation Sans fallbacks, or an explicit `--font` path.
  Fonts are not bundled.
- Focused app name centered on the output, truncated or hidden if space is tight.
  Workspace buttons stay anchored on the left regardless of the app name.
- Workspace buttons, hover state, active workspace, and urgent indicators.
  Click to switch through Sway IPC. Workspace and window events refresh the bar immediately. Workspaces are filtered when using `--output`.
- Hover a workspace for 220 ms to open a live miniature of its window layout.
  Previews use Sway 1.12 individual-window capture, including hidden workspaces,
  and refresh about four times per second while hovered. They never switch
  workspaces or take keyboard focus. Selected tabs, fullscreen and floating
  windows are respected; wallpaper and compositor decorations are not captured.
  Apps may throttle their own updates while hidden. Unavailable captures show
  window placeholders. Older Sway versions without capture identifiers show
  the window layout and titles instead of incorrectly reporting an empty workspace;
  live hidden-window images require Sway 1.12+ and its capture protocols. Image data stays in memory; capture stops on pointer leave.
- Sound button with master volume, mute, output selection, available speaker/headphone
  ports, and individual channel levels. Master adjustments preserve channel balance;
  selecting an output also moves current playback. Click or drag a level to set it; scroll
  the panel to reach additional outputs or channels. Requires `pactl` and PulseAudio
  or PipeWire-Pulse. Audio commands run on a worker and external changes refresh
  within two seconds. Slider thumbs follow the pointer immediately; audio updates
  are coalesced while dragging and always apply the final release position.
- Battery percentage and charging state from Linux sysfs, with a green vector
  lightning bolt while charging (no icon-font dependency).
- Current song and artist from playing MPRIS players. Paused song text hides
  as soon as the player signals the change, leaving a fixed-position music button.
  Playback uses D-Bus events, with a five-second recovery poll.
  Click the button or track label for a floating previous/play-pause/next panel.
  Album artwork comes from MPRIS `mpris:artUrl` (local files or HTTP/HTTPS),
  with rounded corners and a subtle cover-derived tint. PNG, JPEG, and WebP
  load on a separate worker with an eight-cover memory cache; unavailable art
  shows a music-note placeholder. Loads have byte/decode limits and network timeouts.
  Unsupported player actions are disabled. All playback icons are drawn vectors.
  When no player has a track, the music button is hidden.
- Blue audio marks on workspaces with active, unmuted PulseAudio/PipeWire-Pulse
  streams. Stream process IDs (including child processes) are matched to Sway
  windows. Apps with windows on multiple workspaces can light multiple markers;
  this detects active streams, not the waveform's instantaneous loudness.
- Network link indicator: bright when a non-loopback interface reports `up`;
  dim otherwise. The label shows the current Wi-Fi name, with a custom-drawn Wi-Fi
  symbol whose arcs indicate signal strength. Click it for a native floating Wi-Fi panel: current
  connection, nearby networks, signal/security, scan, radio toggle, disconnect,
  and password entry. Saved credentials can be used by leaving the password blank.
  The bar's link indicator is not an internet-connectivity check.
- **Apps** opens a native launcher. Type to filter installed desktop apps, use
  Up/Down to select, Enter to launch, and Escape or an outside click to dismiss.
  Held keys repeat at the compositor's configured rate, including Backspace.
- The four-square mark opens the session menu, including launcher, lock, and logout.
  Lock uses `~/.local/bin/lock-session` when executable, falling back to `swaylock`.
  Logging out requires clicking a second confirmation button inside the menu.
- Floating panels stay open while the cursor is over the bar or the panel.
  They dismiss after two seconds outside both, with a 140 ms fade and 1% shrink.
  Workspace previews also stay open while traversing the bar until the pointer
  leaves it, another workspace is hovered, or a menu opens.
  Reentering cancels dismissal. Password entry, typed launcher searches, pending
  actions, and logout confirmation stay open until dismissed or completed.
- Local date and time, integer HiDPI scaling, and one-second system status updates.
- Status reads and serialized workspace commands happen on a worker thread, woken
  immediately by clicks and Sway events. Rendering is triggered by updates and
  pointer interaction, rather than a continuous animation loop.

This is an initial working bar, not a complete macOS menu-bar implementation.
There are no per-application menus, StatusNotifier tray,
font fallback/shaping for complex scripts, or automatic output hotplug management
yet. Enterprise/hidden Wi-Fi provisioning is not implemented in the popup.
Focused app IDs are displayed as supplied
by Sway, not resolved through desktop files. On narrow outputs, workspace buttons
that do not fit are omitted. Standard Sway does not blur the background; this bar
uses alpha transparency. Fractional output scales use integer buffer scaling.

The implementation uses [Smithay Client Toolkit](https://github.com/Smithay/client-toolkit)
for Wayland, tiny-skia for shapes, and ab_glyph for text. Application and rendering
code is Rust; normal Linux system interfaces remain part of the runtime.

## Preview and validation

```sh
cargo run -- --preview preview-light.png
cargo run -- --dark --preview preview-dark.png
cargo run -- --smoke-test
cargo run -- --check-network
cargo run -- --check-media
cargo test
cargo clippy --all-targets -- -D warnings
```

Previews use fixed sample data and work without Wayland. The smoke test briefly
creates a real layer surface, submits a buffer with live Sway status, and exits without changing your
Sway configuration. It requires access to your Wayland session socket. Tests
cover focused-window discovery, scaled buffers, pixel alpha validity, and
workspace hit-region bounds, title-independent workspace positions, Wi-Fi parsing,
iwd signal conversion, and launcher visibility rules.

`examples/ui_probe.rs` is an opt-in manual Wayland input helper; it moves and
clicks the real pointer, so it is never run by `cargo test`.

## Code

- `src/main.rs`: Wayland lifecycle, input, CLI, worker and event loop.
- `src/render.rs`: themes, text rasterization, layout, icons, hit regions.
- `src/status.rs`: bounded Sway IPC requests and Linux status collection.

## iwd access

Your session must have access to `net.connman.iwd` on the system D-Bus.
`integration/iwd-jordan.conf` is the user-specific policy prepared for this
machine and installed with explicit approval. It grants user `jordan` access
only to the existing iwd service, without changing networking services or groups.
On another machine, use its existing iwd access policy or adapt the username.

Wi-Fi scan/connect jobs run outside the UI loop. iwd passwords are passed through
a short-lived D-Bus agent that checks the daemon sender and target network;
NetworkManager passwords go to nmcli over stdin. Neither path puts passwords in
command-line arguments or logs. Password text is masked; clipboard paste is not
implemented yet. iwd may remember credentials after a successful connection.

Additional modules: `network.rs` selects the backend; `iwd.rs` implements native
iwd integration; `panel.rs` draws popups; `popup_ui.rs` handles popup input;
`launcher.rs` discovers desktop applications; `media.rs` reads playback metadata
and resolves audio streams to workspaces.
