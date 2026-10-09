# bharta

A compact, macOS-inspired Sway bar in Rust. egui supplies the widgets; a direct
Wayland layer surface and CPU renderer display them without GTK, layer-shell
shared libraries, OpenGL, or Vulkan. The bar reserves 28 logical pixels.

## Build and run

```sh
./build.sh
./dist/bharta --dark --all-outputs
```

The helper produces a **single static executable** for x86_64 or aarch64 Linux.
Copy `dist/bharta` to another machine of the same architecture: it needs no
shared libraries, library directory, or distro-specific build. musl is embedded;
keyboard layout handling, Wayland transport, and rendering are Rust. Compose
rules and UI fonts have bundled fallbacks.

Building requires Bash, curl, tar, xz, and sha256sum. The helper provisions Rust
1.92 when needed, installs its musl target, and downloads a pinned,
checksum-verified Zig compiler into `.build/`. Zig supplies the compiler and
headers for the HTTPS crypto code that gets linked into the executable.
No system compiler, development libraries, pkg-config, or root access is needed;
the helper never installs system packages. Downloads are cached for later builds.
An existing Rust installation must be managed by rustup.

```sh
./build.sh --check       # report prerequisites without changing anything
./build.sh --no-install  # same static build; retained for existing scripts
nix-shell --run './build.sh'
```

Ubuntu 22.04, Ubuntu 24.04, and Fedora have clean-image build CI with only
bootstrap tools installed. CI checks that the executable has no ELF interpreter
or shared-library dependencies and uploads an archive preserving its executable
permission. A plain
`cargo build` uses Rust's platform default for development; use `./build.sh`
for distribution. `--font /path/to/font.ttf` selects a custom UI font with the
bundled fallbacks. SF Mono is selected automatically when installed;
`--font` takes precedence.

At runtime, use Sway. Audio requires `pactl` and `parec` (usually
`pulseaudio-utils`, or `libpulse` on Arch), with PulseAudio or PipeWire-Pulse.
Wi-Fi uses iwd over D-Bus or NetworkManager through `nmcli`. Apps uses `gio launch`.
The PNG sample exporter uses installed fonts or `--font`.

```sway
exec /path/to/bharta --dark --all-outputs
```

Use `exec`, remove the old swaybar block, and keep your existing bharta arguments.
By default a supervisor manages all active outputs, handles hotplug, and restarts
bars. `--output DP-1` selects one output. Only one supervisor runs per Sway session.
The default theme is light; `--dark` selects neutral charcoal.

## Controls

Menus open after a 220ms hover or a click. Only one menu opens across all outputs.
Moving to another bar control switches menus. Leaving the bar and panel starts a
500ms delay and a brief fade; input and pending actions retain the relevant menu.
Escape or an outside click dismisses menus. Clicking a hovered menu retains it;
clicking again closes it until the pointer leaves that control.

- Workspaces: click to switch, hover for a bounded miniature layout. Active,
  urgent, and audible workspaces have distinct accents and a reserved speaker
  slot. Live window captures require Sway 1.12+ and its capture protocols; older
  Sway shows positions and titles. Hidden tabs and fullscreen windows are
  respected. Previews use area downsampling with bilinear display filtering.
  Captures run concurrently on damage, reuse sessions/buffers, and target 60 FPS;
  presentation follows compositor frame callbacks, and changed thumbnails invalidate
  their raster cache even after the opening fade. Capture stays in memory and
  stops when the preview closes.
- Sound: individual app streams, per-stream volume/mute, source-specific
  frequency meters, MPRIS playback controls and artwork, output/port selection,
  master volume/mute, and expandable channels. Audio sliders use compact neutral
  tracks and round handles without percentage readouts. Paused sources retain their
  sliders and row height. Meters monitor their own streams through `parec` and
  stop when the drawer closes. Master volume preserves channel balance; changing
  output moves current playback. Worker commands are ordered and slider updates
  are coalesced. Playback changes keep the speaker button in place.
- Wi-Fi: aligned signal percentages, live connected RSSI on iwd, radio switch,
  scan, connect/disconnect, saved credentials, and a password
  field with reveal and clipboard paste. Enterprise and hidden networks can be
  provisioned in system network settings.
- Apps: immediately focused search, desktop-entry filtering, arrow-key selection,
  Enter to launch, or clickable rows. egui handles editing and selection; Wayland
  handles clipboard transfer and keyboard layout/repeat.
- Session: lock using `~/.local/bin/lock-session`, falling back to `swaylock`;
  logout requires two clicks.
- Battery percentage, charging indicator, and local clock.

For iwd, the session needs access to `net.connman.iwd` on the system D-Bus.
`integration/iwd-jordan.conf` is an example policy with a machine-specific user;
adapt it to your existing policy. Passwords use a temporary iwd D-Bus agent or
NetworkManager stdin, never command-line arguments or logs.

There are no per-application menus or StatusNotifier tray. Standard Sway does not
blur the bar. Desktop portal screen sharing is separate from window previews.

## Code and validation

`src/ui/mod.rs` contains the egui bar and menus; `wayland.rs` supplies layer-shell,
software presentation, keyboard, pointer, and clipboard; `services.rs` connects
background workers. Audio, media, Wi-Fi, launcher, capture, and Sway backends
remain independent. Edit Rust styling and rebuild; no CSS or GTK bindings remain.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release --examples
cargo build --release
python3 tests/headless_ui.py /tmp/bharta-headless
```

The harness requires Sway, grim, dbus-daemon, and Python with Pillow. It uses a
private headless display and D-Bus session, fake audio streams with distinct
tones, and two MPRIS fixtures with distinct artwork. It checks continuous dragging,
hover retention/switching, dismissal, preview bounds, meter isolation, stable
paused controls, and one menu across outputs. On Sway 1.12+, it also benchmarks
an animated capture alongside a static source (45 FPS minimum under headless
testing, with no repeated captures of the static source). Inspect its screenshots
and logs.
It never sends input to the desktop or changes real audio levels.

`--smoke-test` maps a layer surface for three seconds and requires Wayland.
`--check-network` and `--check-media` diagnose the backends. Some unit tests need
local IPC permissions. The ignored live audio test creates a temporary null sink
and must be explicitly enabled. `--preview FILE.png` remains a headless sample
exporter using the original renderer; capture the live egui UI with `grim`.
