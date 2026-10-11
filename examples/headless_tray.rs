//! SNI/dbusmenu fixtures. Run only on the harness's private session bus.
use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    sync::{Arc, Mutex},
};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
struct State {
    status: String,
    title: String,
    color: [u8; 4],
}
fn log(action: &str) {
    if let Ok(path) = std::env::var("BHARTA_TEST_TRAY_LOG") {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .unwrap();
        writeln!(file, "{action}").unwrap();
    }
}
macro_rules! item {
    ($name:ident, $interface:literal) => {
        struct $name(Arc<Mutex<State>>);
        #[zbus::interface(name = $interface)]
        impl $name {
            #[zbus(property)]
            fn status(&self) -> String {
                self.0.lock().unwrap().status.clone()
            }
            #[zbus(property)]
            fn title(&self) -> String {
                self.0.lock().unwrap().title.clone()
            }
            #[zbus(property)]
            fn id(&self) -> &str {
                $interface
            }
            #[zbus(property)]
            fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
                vec![(22, 22, self.0.lock().unwrap().color.repeat(22 * 22))]
            }
            #[zbus(property)]
            fn menu(&self) -> OwnedObjectPath {
                "/Menu".try_into().unwrap()
            }
            #[zbus(property)]
            fn item_is_menu(&self) -> bool {
                false
            }
            fn activate(&self, _x: i32, _y: i32) {
                log(concat!($interface, ":activate"));
            }
            fn secondary_activate(&self, _x: i32, _y: i32) {
                log(concat!($interface, ":secondary"));
            }
            fn context_menu(&self, _x: i32, _y: i32) {
                log(concat!($interface, ":context"));
            }
            fn scroll(&self, delta: i32, orientation: &str) {
                log(&format!("{orientation}:{delta}"));
            }
        }
    };
}
item!(KdeItem, "org.kde.StatusNotifierItem");
item!(FdoItem, "org.freedesktop.StatusNotifierItem");
struct Menu;
fn props(label: &str) -> HashMap<String, OwnedValue> {
    HashMap::from([
        ("label".into(), zbus::zvariant::Str::from(label).into()),
        ("enabled".into(), true.into()),
        ("visible".into(), true.into()),
    ])
}
type Layout = (i32, HashMap<String, OwnedValue>, Vec<Value<'static>>);
#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn about_to_show(&self, id: i32) -> bool {
        log(&format!("about:{id}"));
        false
    }
    fn get_layout(&self, _parent_id: i32, _depth: i32, _properties: Vec<String>) -> (u32, Layout) {
        let mut disabled = props("Disabled");
        disabled.insert("enabled".into(), false.into());
        let mut checked = props("Checked");
        checked.insert(
            "toggle-type".into(),
            zbus::zvariant::Str::from("checkmark").into(),
        );
        checked.insert("toggle-state".into(), 1i32.into());
        (
            1,
            (
                0,
                HashMap::new(),
                vec![
                    Value::from((1i32, props("_Open app"), Vec::<Value>::new())),
                    Value::from((2i32, disabled, Vec::<Value>::new())),
                    Value::from((3i32, checked, Vec::<Value>::new())),
                    Value::from((
                        4i32,
                        props("More"),
                        vec![Value::from((
                            5i32,
                            props("Nested action"),
                            Vec::<Value>::new(),
                        ))],
                    )),
                ],
            ),
        )
    }
    fn event(&self, id: i32, event: &str, _data: Value<'_>, _timestamp: u32) {
        log(&format!("event:{id}:{event}"));
    }
}
fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var("BHARTA_HEADLESS").as_deref() == Ok("1"),
        "Only run from the headless harness"
    );
    let kde = Arc::new(Mutex::new(State {
        status: "Active".into(),
        title: "Tray Alpha".into(),
        color: [255, 220, 40, 70],
    }));
    let fdo = Arc::new(Mutex::new(State {
        status: "Active".into(),
        title: "Tray Beta".into(),
        color: [255, 40, 170, 220],
    }));
    let a = zbus::blocking::connection::Builder::session()?
        .name("org.bharta.TrayFixture")?
        .serve_at("/CustomTray", KdeItem(kde.clone()))?
        .serve_at("/Menu", Menu)?
        .build()?;
    let b = zbus::blocking::connection::Builder::session()?
        .name("org.bharta.LegacyTrayFixture")?
        .serve_at("/StatusNotifierItem", FdoItem(fdo))?
        .serve_at("/Menu", Menu)?
        .build()?;
    let mut registered = false;
    for _ in 0..100 {
        if let Ok(proxy) = zbus::blocking::Proxy::new(
            &a,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        ) && proxy
            .call::<_, _, ()>("RegisterStatusNotifierItem", &("/CustomTray",))
            .is_ok()
        {
            registered = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::ensure!(registered, "No watcher");
    let watcher = zbus::blocking::Proxy::new(
        &b,
        "org.freedesktop.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.freedesktop.StatusNotifierWatcher",
    )?;
    watcher.call::<_, _, ()>(
        "RegisterStatusNotifierItem",
        &("org.bharta.LegacyTrayFixture",),
    )?;
    // Duplicate registration under another spelling must not duplicate icons.
    zbus::blocking::Proxy::new(
        &a,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    )?
    .call::<_, _, ()>("RegisterStatusNotifierItem", &("/CustomTray",))?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line == "quit" {
            break;
        }
        let mut state = kde.lock().unwrap();
        match line.as_str() {
            "passive" => state.status = "Passive".into(),
            "active" => state.status = "Active".into(),
            "change" => {
                state.title = "Changed title".into();
                state.color = [255, 80, 220, 100];
            }
            _ => {}
        }
        drop(state);
        a.emit_signal(
            None::<&str>,
            "/CustomTray",
            "org.kde.StatusNotifierItem",
            "NewStatus",
            &(kde.lock().unwrap().status.clone(),),
        )?;
        a.emit_signal(
            None::<&str>,
            "/CustomTray",
            "org.kde.StatusNotifierItem",
            "NewIcon",
            &(),
        )?;
    }
    Ok(())
}
