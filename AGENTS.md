# Working on bharta

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
dragging must work continuously. Preserve the fixed music icon position on pause
and bound workspace previews independently of captured image dimensions.
All menus open on a short hover; leaving the bar and popup starts a 500ms delay
followed by a brief fade. Keep Wi-Fi names and actions left-aligned. Only one
popup may be open across all outputs at a time. Hovering another button must
switch directly to its menu, including workspace previews and clicked menus.
Apps focuses its search field on opening, including hover, so typing works immediately.

Commit completed changes as requested by the user. No confirmation is needed
for ordinary code edits.
