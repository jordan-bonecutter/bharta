# bharta

A compact, macOS-inspired GTK4 status bar for Sway, written in Rust. GTK supplies native
widgets, text shaping, accessibility, clipboard handling, and scaling;
`gtk4-layer-shell` anchors the bar to each display.

## Run

Requires a current Rust toolchain, Sway, GTK4 (4.8+), gtk4-layer-shell, Fontconfig,
and libxkbcommon. On Arch, the additional UI dependencies are `gtk4` and
`gtk4-layer-shell`. Install these on each machine before building or copying the
binary. Audio controls require `pactl` and PulseAudio or PipeWire-Pulse. Wi-Fi uses
iwd over D-Bus or NetworkManager through `nmcli`; launching apps uses `gio launch`.

```sh
cargo build --release
./target/release/bharta --dark --all-outputs
./target/release/bharta --output DP-1
./target/release/bharta --font /path/to/font.ttf
```

The existing command line and Sway startup configuration continue to work:

```sway
exec /home/jordan/Projects/bharta/target/release/bharta --dark --all-outputs
```

Use `exec`, not `exec_always`, and remove the old swaybar block. By default, a
supervisor manages one bar per active output, detects hotplug, and restarts exited
bars. `--output NAME` selects one output. Only one supervisor runs per Sway session.
The bar reserves 28 logical pixels. The default theme is light; `--dark` selects
the original neutral charcoal palette. Fonts fall back through GTK; `--font` registers and
selects a font file using Fontconfig.

## Controls

All menus open on hover (220ms) or click. Apps focuses its search field immediately;
other menus do not take keyboard focus merely from hovering. Only one popup is shown across all outputs at a time. Leaving both the bar and popup starts a half-second close delay.

- Click workspaces to switch; hover for a miniature window layout. Active,
  urgent, and audible workspaces have distinct accents. Individual-window live
  captures require Sway 1.12+ and its capture protocols. Older Sway, including
  1.9, shows window positions and titles. Hidden tabs and fullscreen windows are
  respected. Capture runs only while a preview is open and stays in memory.
- **Sound** has a native draggable volume scale, mute, output and port selection,
  and an expandable channel section. Master volume preserves channel balance;
  changing output moves current playback. Commands run on a worker, slider
  updates are coalesced, and external audio changes refresh within two seconds.
- Hover or click **Wi-Fi** for a radio switch, scan, connection/disconnection, and a native
  password entry with clipboard paste and a reveal button. Leave the password
  blank to use saved credentials. Enterprise/hidden network provisioning still
  belongs in your system network settings.
- **Apps** filters installed desktop entries. Use the keyboard or click a row
  to launch. GTK supplies text editing, selection, key repeat, and clipboard use.
- Hover or click the music icon or track text for a menu showing artwork, title, artist, and supported playback controls.
  Seven bars show measured frequency bands from the default audio output while
  music is playing (other sounds on that output contribute too). This uses the
  PulseAudio monitor interface (`parec`), which
  also works with PipeWire-Pulse. Install the distro package that provides `parec`
  (usually `pulseaudio-utils`); the panel remains usable without it. Audio is
  processed in memory and capture ends when playback pauses or the panel closes. MPRIS events update playback immediately, with recovery polling. Artwork loads
  on a bounded background worker. Paused tracks keep the music button visible.
- The session menu offers lock and a two-click logout confirmation. Lock uses
  `~/.local/bin/lock-session`, falling back to `swaylock`.
- Battery percentage includes a charging indicator. The clock uses local time.
- Menus stay open over the bar or panel, then fade after half a second outside. Menus also fade in and fade out on dismissal.
  Input and pending actions pin relevant menus. Escape or an outside click closes
  interactive menus. Workspace previews do not request keyboard focus.

## Customization

Edit `src/gtk_ui/theme.css` for spacing, typography, and widget styling;
`dark.css` and `light.css` contain the palettes. GTK widgets handle layout and
interaction, so changes do not require updating separate painted hit regions.
Rebuild after editing these embedded stylesheets.

`src/gtk_ui/mod.rs` owns the layer-shell window and UI state; `sound.rs` contains
the persistent scales; `music.rs` and `preview.rs` render media and workspace
previews; `menus.rs` contains launcher, network, and session widgets. `services.rs` connects the existing background workers to
the GTK main context. Sway IPC, audio, Wi-Fi, launcher, capture, and artwork
backends remain separate from the widget code.

## Validation

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo run -- --smoke-test
cargo run -- --check-network
cargo run -- --check-media
```

The smoke test maps a GTK layer surface for three seconds and needs access to the
Wayland session. Some tests create local IPC/D-Bus sockets. The ignored live audio
test creates a temporary null sink and must be explicitly enabled. The
`examples/ui_probe.rs` helper is restricted to the isolated headless harness.

`--preview FILE.png` is retained as a headless sample exporter using the original
software renderer. It illustrates sample status data; it is not a screenshot of
the GTK interface. Capture the running bar with `grim` to inspect the actual UI.

## iwd access

The session needs access to `net.connman.iwd` on the system D-Bus.
`integration/iwd-jordan.conf` is a policy for this machine's user; another machine
should use its own existing policy or adapt the username. Wi-Fi jobs run outside
the UI loop. iwd passwords go through a temporary D-Bus agent that verifies the
daemon and target network; NetworkManager passwords go through stdin, never
command-line arguments or logs. iwd may remember credentials after connecting.

There are no per-application menus or StatusNotifier tray. Standard Sway does
not blur the translucent bar. Screen sharing in Teams uses desktop portals and
is separate from bharta's individual-window previews.

### Headless UI checks

```sh
cargo build --release --examples
cargo build --release
python3 tests/headless_ui.py /tmp/bharta-headless
```

Requires Python with Pillow, Sway, grim, and dbus-daemon. The harness uses its own
headless display, private D-Bus session, and fake audio backend. Screenshots and
logs go to the supplied directory. It checks dragging, hover opening and retention,
dismissal, captured-window preview bounds, stable playback controls on pause,
and single-popup coordination across two outputs. It never injects input into your
normal desktop or changes your audio. See `AGENTS.md` for the required agent
workflow: UI automation must use this harness, not the live session.
