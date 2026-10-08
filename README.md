# bharta

A compact, macOS-inspired GTK4 status bar for Sway, written in Rust. GTK supplies native
widgets, text shaping, accessibility, clipboard handling, and scaling;
`gtk4-layer-shell` anchors the bar to each display.

## Build and run

From a fresh clone, run:

```sh
./build.sh
./target/release/bharta --dark --all-outputs
```

The build command checks the complete dependency set up front. On Ubuntu/Debian,
Fedora, Arch, openSUSE, Alpine, and Void, it installs missing development packages
in one package-manager invocation (using `sudo` when needed). It provisions Rust
1.92 if the existing compiler is too old. GTK4 layer-shell is built from a pinned,
checksum-verified upstream release into `.build/native` when the distro does not
provide it; there is no system library installation, PPA, or AUR requirement.
Docs, Vala, and introspection generators are disabled for that private library,
because the Rust bar does not need them. If Rust was installed by the helper,
`source ~/.cargo/env` enables manual Cargo commands in your current shell.

```sh
./build.sh --check       # report prerequisites without installing/downloading
./build.sh --no-install  # build with existing system packages; provision private deps
nix-shell --run './build.sh --no-install'  # NixOS / Nix package manager
```

Native builds require Bash, GTK4 4.6+, Rust 1.92+, a C compiler, pkg-config, Fontconfig,
libxkbcommon, and Wayland development files. Ubuntu 22.04 and 24.04 are covered by
clean-image build CI, alongside Fedora. Older systems with GTK below 4.6 need a
newer build environment; this script does not replace a distro's GTK libraries.
Other distros receive the complete prerequisite list rather than a sequence of
Cargo build failures. Nix needs a current nixpkgs channel providing Rust 1.92+.

The result keeps the original `target/release/bharta` path, so existing Sway startup
commands still work. When the private layer-shell fallback is used, its library
is copied to `target/release/lib`, and the executable finds it through a relative
runpath. Keep that directory beside the executable when moving it. GTK and the
other native libraries must still be installed on the destination; this is a
native build, not a universal standalone binary. `.build` contains reusable
private build dependencies and can be deleted to start fresh.

If dependencies are already installed, `cargo build --release --locked` also
works with a sufficiently recent Rust toolchain and system GTK4 layer-shell 1.0+.
Use the helper when the system does not package that library.

At runtime, Sway provides the compositor. Audio controls and meters require
`pactl`/`parec` (usually `pulseaudio-utils`, or `libpulse` on Arch) and PulseAudio
or PipeWire-Pulse. Wi-Fi uses iwd over D-Bus or NetworkManager through `nmcli`;
launching apps uses `gio launch`.

```sh
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
- The speaker button opens one audio drawer with a live stack of app streams,
  per-app volume and mute, source-specific frequency meters, output and port
  selection, master mute/volume, and expandable channel controls. Browser rows
  include their media title where available. A single source keeps its name in
  the bar; multiple active streams show a source count. The live meters use
  PulseAudio-compatible per-stream monitoring through `parec`, and run only
  while the drawer is open. The distro package providing `parec` is usually
  `pulseaudio-utils`. Master volume preserves channel balance; changing output
  moves current playback. Commands run on a worker and slider updates are
  coalesced.
- Hover or click **Wi-Fi** for a radio switch, scan, connection/disconnection, and a native
  password entry with clipboard paste and a reveal button. Leave the password
  blank to use saved credentials. Enterprise/hidden network provisioning still
  belongs in your system network settings.
- **Apps** filters installed desktop entries. Use the keyboard or click a row
  to launch. GTK supplies text editing, selection, key repeat, and clipboard use.
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
the persistent audio controls and per-source meters; `preview.rs` renders workspace
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
