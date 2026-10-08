//! Headless UI input: output, x, y, width, height, then optional evdev key codes.
//! Only run on the isolated harness compositor; never on the desktop session.
use std::{collections::HashMap, io::Write, os::fd::AsFd, time::Duration};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output, wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};
#[derive(Default)]
struct State {
    outputs: HashMap<wayland_client::backend::ObjectId, String>,
}
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        s: &mut Self,
        o: &wl_output::WlOutput,
        e: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = e {
            s.outputs.insert(o.id(), name);
        }
    }
}
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);
fn timestamp() -> u32 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Wayland input timestamps use the compositor's monotonic clock.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time);
    }
    (time.tv_sec as u64 * 1000 + time.tv_nsec as u64 / 1_000_000) as u32
}
fn main() -> anyhow::Result<()> {
    let a: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        a.len() >= 5,
        "Usage: ui_probe OUTPUT X Y WIDTH HEIGHT [--hover [HOLD_MS] | --drag END_X END_Y | EVDEV_KEY ...]"
    );
    anyhow::ensure!(
        std::env::var("BHARTA_HEADLESS").as_deref() == Ok("1") && a[0].starts_with("HEADLESS-"),
        "ui_probe requires the isolated headless harness"
    );
    let c = Connection::connect_to_env()?;
    let (g, mut q) = registry_queue_init::<State>(&c)?;
    let h = q.handle();
    let mut s = State::default();
    let outputs: Vec<wl_output::WlOutput> = g.contents().with_list(|l| {
        l.iter()
            .filter(|x| x.interface == "wl_output")
            .map(|x| g.registry().bind(x.name, x.version.min(4), &h, ()))
            .collect()
    });
    q.roundtrip(&mut s)?;
    let output = outputs
        .iter()
        .find(|o| s.outputs.get(&o.id()) == Some(&a[0]))
        .ok_or_else(|| anyhow::anyhow!("Output not found"))?;
    let seat: wl_seat::WlSeat = g.bind(&h, 1..=7, ())?;
    let manager: ZwlrVirtualPointerManagerV1 = g.bind(&h, 2..=2, ())?;
    let pointer = manager.create_virtual_pointer_with_output(Some(&seat), Some(output), &h, ());
    pointer.motion_absolute(
        timestamp(),
        a[1].parse()?,
        a[2].parse()?,
        a[3].parse()?,
        a[4].parse()?,
    );
    pointer.frame();
    q.roundtrip(&mut s)?;
    std::thread::sleep(Duration::from_millis(150));
    if a.get(5).is_some_and(|arg| arg == "--hover") {
        // Keep the virtual pointer alive for hover assertions. Destroying it
        // can restore the physical pointer position and generate a leave.
        if let Some(hold) = a.get(6) {
            let until = std::time::Instant::now() + Duration::from_millis(hold.parse()?);
            while std::time::Instant::now() < until {
                // Keep pointer focus current after a popup releases its grab.
                pointer.motion_absolute(
                    timestamp(),
                    a[1].parse()?,
                    a[2].parse()?,
                    a[3].parse()?,
                    a[4].parse()?,
                );
                pointer.frame();
                c.flush()?;
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        pointer.destroy();
        c.flush()?;
        return Ok(());
    }
    pointer.button(timestamp(), 0x110, wl_pointer::ButtonState::Pressed);
    pointer.frame();
    q.roundtrip(&mut s)?;
    std::thread::sleep(Duration::from_millis(150));
    let dragging = a.get(5).is_some_and(|arg| arg == "--drag");
    if dragging {
        anyhow::ensure!(a.len() == 8, "--drag needs END_X END_Y");
        let (x, y): (f64, f64) = (a[1].parse()?, a[2].parse()?);
        let (end_x, end_y): (f64, f64) = (a[6].parse()?, a[7].parse()?);
        for step in 1..=24 {
            let t = step as f64 / 24.;
            pointer.motion_absolute(
                timestamp(),
                (x + (end_x - x) * t) as u32,
                (y + (end_y - y) * t) as u32,
                a[3].parse()?,
                a[4].parse()?,
            );
            pointer.frame();
            q.roundtrip(&mut s)?;
            std::thread::sleep(Duration::from_millis(30));
        }
        // Allow assertions against the server while the button is still held.
        std::thread::sleep(Duration::from_millis(500));
    }
    pointer.button(timestamp(), 0x110, wl_pointer::ButtonState::Released);
    pointer.frame();
    q.roundtrip(&mut s)?;
    if a.get(5).is_some_and(|arg| arg == "--click-hold") {
        std::thread::sleep(Duration::from_millis(
            a.get(6).map(|v| v.parse()).transpose()?.unwrap_or(2000),
        ));
    } else if a.len() > 5 && !dragging {
        std::thread::sleep(Duration::from_millis(500));
        let km = xkbcommon::xkb::Keymap::new_from_names(
            &xkbcommon::xkb::Context::new(0),
            "",
            "",
            "us",
            "",
            None,
            0,
        )
        .unwrap();
        let text = km.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1) + "\0";
        let mut file = tempfile::tempfile()?;
        file.write_all(text.as_bytes())?;
        let manager: ZwpVirtualKeyboardManagerV1 = g.bind(&h, 1..=1, ())?;
        let keyboard = manager.create_virtual_keyboard(&seat, &h, ());
        keyboard.keymap(1, file.as_fd(), text.len() as u32);
        q.roundtrip(&mut s)?;
        std::thread::sleep(Duration::from_millis(200));
        if a[5] == "--keyboard-hold" {
            std::thread::sleep(Duration::from_millis(a[6].parse()?));
        } else {
            for key in &a[5..] {
                let (code, hold) = key.split_once('@').unwrap_or((key, "0"));
                keyboard.key(timestamp(), code.parse()?, 1);
                q.roundtrip(&mut s)?;
                std::thread::sleep(Duration::from_millis(hold.parse::<u64>()?.max(50)));
                keyboard.key(timestamp(), code.parse()?, 0);
                q.roundtrip(&mut s)?;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        keyboard.destroy();
    }
    pointer.destroy();
    c.flush()?;
    Ok(())
}
