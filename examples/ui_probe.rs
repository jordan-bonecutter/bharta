//! Manual UI check: output, x, y, width, height, then optional evdev key codes.
//! Moves/clicks a real desktop pointer; never run as part of unattended tests.
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
fn main() -> anyhow::Result<()> {
    let a: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        a.len() >= 5,
        "Usage: ui_probe OUTPUT X Y WIDTH HEIGHT [EVDEV_KEY ...]"
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
        1,
        a[1].parse()?,
        a[2].parse()?,
        a[3].parse()?,
        a[4].parse()?,
    );
    pointer.frame();
    q.roundtrip(&mut s)?;
    std::thread::sleep(Duration::from_millis(150));
    pointer.button(2, 0x110, wl_pointer::ButtonState::Pressed);
    pointer.frame();
    q.roundtrip(&mut s)?;
    std::thread::sleep(Duration::from_millis(150));
    pointer.button(3, 0x110, wl_pointer::ButtonState::Released);
    pointer.frame();
    q.roundtrip(&mut s)?;
    if a.len() > 5 {
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
        for key in &a[5..] {
            let (code, hold) = key.split_once('@').unwrap_or((key, "0"));
            keyboard.key(10, code.parse()?, 1);
            q.roundtrip(&mut s)?;
            std::thread::sleep(Duration::from_millis(hold.parse()?));
            keyboard.key(11, code.parse()?, 0);
            q.roundtrip(&mut s)?;
        }
        std::thread::sleep(Duration::from_millis(300));
        keyboard.destroy();
    }
    pointer.destroy();
    c.flush()?;
    Ok(())
}
