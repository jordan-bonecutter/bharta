//! StatusNotifier host, with an optional XEmbed-to-SNI compatibility bridge.
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedValue, Value},
};
const PATH: &str = "/StatusNotifierWatcher";
const WATCHERS: [&str; 2] = [
    "org.kde.StatusNotifierWatcher",
    "org.freedesktop.StatusNotifierWatcher",
];
type Registry = Arc<Mutex<BTreeMap<String, String>>>;

macro_rules! watcher {
    ($ty:ident, $interface:literal) => {
        struct $ty(Registry);
        #[zbus::interface(name = $interface)]
        impl $ty {
            async fn register_status_notifier_item(
                &self,
                service: &str,
                #[zbus(header)] header: zbus::message::Header<'_>,
                #[zbus(connection)] connection: &zbus::Connection,
            ) -> zbus::fdo::Result<()> {
                let sender = header
                    .sender()
                    .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Missing sender".into()))?;
                let (destination, path) = if service.starts_with('/') {
                    (sender.as_str(), service)
                } else {
                    (service, "/StatusNotifierItem")
                };
                zbus::names::BusName::try_from(destination)
                    .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
                zbus::zvariant::ObjectPath::try_from(path)
                    .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
                let bus = zbus::fdo::DBusProxy::new(connection).await?;
                let owner = bus.get_name_owner(destination.try_into().unwrap()).await?;
                let id = format!("{destination}{path}");
                let inserted = self
                    .0
                    .lock()
                    .unwrap()
                    .insert(id.clone(), owner.to_string())
                    .is_none();
                if inserted {
                    // Both interfaces share the registry, so notify hosts using either spelling.
                    for interface in WATCHERS {
                        connection
                            .emit_signal(
                                None::<&str>,
                                PATH,
                                interface,
                                "StatusNotifierItemRegistered",
                                &(id.as_str(),),
                            )
                            .await?;
                        connection
                            .emit_signal(
                                None::<&str>,
                                PATH,
                                "org.freedesktop.DBus.Properties",
                                "PropertiesChanged",
                                &(
                                    interface,
                                    HashMap::<String, OwnedValue>::new(),
                                    vec!["RegisteredStatusNotifierItems"],
                                ),
                            )
                            .await?;
                    }
                }
                Ok(())
            }
            async fn register_status_notifier_host(
                &self,
                service: &str,
                #[zbus(connection)] connection: &zbus::Connection,
            ) -> zbus::fdo::Result<()> {
                let name = service.try_into().map_err(|e: zbus::names::Error| {
                    zbus::fdo::Error::InvalidArgs(e.to_string())
                })?;
                zbus::fdo::DBusProxy::new(connection)
                    .await?
                    .get_name_owner(name)
                    .await?;
                connection
                    .emit_signal(
                        None::<&str>,
                        PATH,
                        $interface,
                        "StatusNotifierHostRegistered",
                        &(),
                    )
                    .await?;
                Ok(())
            }
            #[zbus(property)]
            fn registered_status_notifier_items(&self) -> Vec<String> {
                self.0.lock().unwrap().keys().cloned().collect()
            }
            #[zbus(property)]
            fn is_status_notifier_host_registered(&self) -> bool {
                true
            }
            #[zbus(property)]
            fn protocol_version(&self) -> i32 {
                0
            }
            // Export signal introspection; emission above also notifies the other spelling.
            #[allow(dead_code)]
            #[zbus(signal)]
            async fn status_notifier_item_registered(
                context: &zbus::SignalContext<'_>,
                service: &str,
            ) -> zbus::Result<()>;
            // Export signal introspection; emission above also notifies the other spelling.
            #[allow(dead_code)]
            #[zbus(signal)]
            async fn status_notifier_item_unregistered(
                context: &zbus::SignalContext<'_>,
                service: &str,
            ) -> zbus::Result<()>;
            // Export signal introspection; emission above also notifies the other spelling.
            #[allow(dead_code)]
            #[zbus(signal)]
            async fn status_notifier_host_registered(
                context: &zbus::SignalContext<'_>,
            ) -> zbus::Result<()>;
        }
    };
}
watcher!(KdeWatcher, "org.kde.StatusNotifierWatcher");
watcher!(FdoWatcher, "org.freedesktop.StatusNotifierWatcher");

#[derive(Clone, Debug, PartialEq)]
pub struct Icon {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub icon: Option<Icon>,
    pub menu: Option<String>,
    pub menu_only: bool,
    interface: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct MenuRow {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub separator: bool,
    pub checked: Option<bool>,
    pub children: Vec<MenuRow>,
}
pub enum Request {
    Activate(String, &'static str, i32, i32),
    Scroll(String, i32, &'static str),
    OpenMenu(Option<String>),
    Click(String, i32),
}
pub enum Update {
    Items(Vec<Item>),
    Menu(String, Result<Vec<MenuRow>, String>),
}

/// Keep the helper scoped to the watcher-owning bar. It is optional and never installed here.
struct Bridge(Option<std::process::Child>);
impl Bridge {
    fn start() -> Self {
        let executable = crate::config::get().text("tray.xembed_bridge");
        let child = if !executable.is_empty() && std::env::var_os("DISPLAY").is_some() {
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(executable);
            command.stdin(std::process::Stdio::null());
            let parent = std::process::id() as libc::pid_t;
            // The supervisor terminates output bars with SIGTERM. Rust destructors
            // do not run then, so also tie the bridge's lifetime to its parent.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(std::io::Error::other("Tray host exited"));
                    }
                    Ok(())
                });
            }
            command.spawn().ok()
        } else {
            None
        };
        Self(child)
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
pub fn watch() -> (mpsc::Sender<Request>, mpsc::Receiver<Update>) {
    let (tx, requests) = mpsc::channel();
    let (events, rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let result = run(&requests, &events);
            if result.is_ok() {
                break;
            }
            // A missing/restarted session bus should not prevent the rest of the bar working.
            if events.send(Update::Items(vec![])).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
    (tx, rx)
}
fn run(requests: &mpsc::Receiver<Request>, events: &mpsc::Sender<Update>) -> anyhow::Result<()> {
    let registry = Registry::default();
    let connection = zbus::blocking::connection::Builder::session()?
        .serve_at(PATH, KdeWatcher(registry.clone()))?
        .serve_at(PATH, FdoWatcher(registry.clone()))?
        .build()?;
    let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    let mut owned = [false; 2];
    let mut hosts = [String::new(), String::new()];
    let mut bridge = None;
    let mut items = Vec::<Item>::new();
    let mut menu = None::<String>;
    let mut last_rows = None;
    loop {
        for (i, name) in WATCHERS.iter().enumerate() {
            if !owned[i] {
                owned[i] = matches!(
                    connection.request_name_with_flags(
                        *name,
                        zbus::fdo::RequestNameFlags::DoNotQueue.into()
                    ),
                    Ok(zbus::fdo::RequestNameReply::PrimaryOwner
                        | zbus::fdo::RequestNameReply::AlreadyOwner)
                );
            }
            if let Ok(owner) = bus.get_name_owner((*name).try_into()?)
                && owner.as_str() != hosts[i]
            {
                let proxy = Proxy::new(&connection, *name, PATH, *name)?;
                let _: Result<(), _> = proxy.call(
                    "RegisterStatusNotifierHost",
                    &(connection.unique_name().unwrap().as_str(),),
                );
                hosts[i] = owner.to_string();
            }
        }
        if owned[0] && bridge.is_none() {
            bridge = Some(Bridge::start());
        }
        let registered = registry.lock().unwrap().clone();
        for (id, owner) in registered {
            let destination = split_id(&id).0;
            if bus
                .get_name_owner(destination.try_into()?)
                .ok()
                .as_ref()
                .map(|n| n.as_str())
                != Some(owner.as_str())
            {
                registry.lock().unwrap().remove(&id);
                for interface in WATCHERS {
                    connection.emit_signal(
                        None::<&str>,
                        PATH,
                        interface,
                        "StatusNotifierItemUnregistered",
                        &(id.as_str(),),
                    )?;
                    connection.emit_signal(
                        None::<&str>,
                        PATH,
                        "org.freedesktop.DBus.Properties",
                        "PropertiesChanged",
                        &(
                            interface,
                            HashMap::<String, OwnedValue>::new(),
                            vec!["RegisteredStatusNotifierItems"],
                        ),
                    )?;
                }
            }
        }
        let mut ids = Vec::new();
        for name in WATCHERS {
            if let Ok(proxy) = Proxy::new(&connection, name, PATH, name)
                && let Ok(registered) =
                    proxy.get_property::<Vec<String>>("RegisteredStatusNotifierItems")
            {
                ids.extend(registered);
            }
        }
        ids.sort();
        ids.dedup();
        ids.truncate(64);
        let mut seen = std::collections::HashSet::new();
        let mut next = Vec::new();
        for id in ids {
            let (destination, path) = split_id(&id);
            let Ok(owner) = bus.get_name_owner(destination.try_into()?) else {
                continue;
            };
            let canonical = format!("{}{path}", owner.as_str());
            if seen.insert(canonical.clone())
                && let Some(item) = read_item(&connection, &canonical)
            {
                next.push(item);
            }
        }
        // Existing items keep their positions through status/metadata changes.
        next.sort_by_key(|item| {
            (
                items
                    .iter()
                    .position(|old| old.id == item.id)
                    .unwrap_or(usize::MAX),
                item.id.clone(),
            )
        });
        if next != items {
            events.send(Update::Items(next.clone()))?;
            items = next;
        }
        if let Some(id) = &menu {
            let rows = items
                .iter()
                .find(|item| &item.id == id)
                .ok_or_else(|| anyhow::anyhow!("Application closed"))
                .and_then(|item| read_menu(&connection, item, false))
                .map_err(|e| e.to_string());
            if last_rows.as_ref() != Some(&rows) {
                events.send(Update::Menu(id.clone(), rows.clone()))?;
                last_rows = Some(rows);
            }
        }
        match requests.recv_timeout(Duration::from_millis(750)) {
            Ok(Request::OpenMenu(id)) => {
                menu = id;
                last_rows = None;
                if let Some(id) = &menu
                    && let Some(item) = items.iter().find(|item| &item.id == id)
                {
                    let rows = read_menu(&connection, item, true).map_err(|e| e.to_string());
                    events.send(Update::Menu(id.clone(), rows.clone()))?;
                    last_rows = Some(rows);
                }
            }
            Ok(request) => {
                if let Err(error) = action(&connection, &items, request) {
                    eprintln!("Tray action: {error}");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}
// An unresponsive tray app must not stall the entire tray worker.
fn bounded<T>(future: impl std::future::Future<Output = zbus::Result<T>>) -> zbus::Result<T> {
    futures_lite::future::block_on(futures_lite::future::or(future, async {
        async_io::Timer::after(Duration::from_millis(600)).await;
        Err(zbus::Error::Failure(
            "Tray application did not respond".into(),
        ))
    }))
}
fn split_id(id: &str) -> (&str, &str) {
    id.find('/')
        .map(|at| (&id[..at], &id[at..]))
        .unwrap_or((id, "/StatusNotifierItem"))
}
fn read_item(connection: &Connection, id: &str) -> Option<Item> {
    let (destination, path) = split_id(id);
    for interface in [
        "org.kde.StatusNotifierItem",
        "org.freedesktop.StatusNotifierItem",
    ] {
        let proxy = Proxy::new(
            connection,
            destination,
            path,
            "org.freedesktop.DBus.Properties",
        )
        .ok()?;
        let Ok(props) = bounded(
            proxy
                .inner()
                .call::<_, _, HashMap<String, OwnedValue>>("GetAll", &(interface,)),
        ) else {
            continue;
        };
        if text(&props, "Status") == "Passive" {
            return None;
        }
        let attention = text(&props, "Status") == "NeedsAttention";
        let pixmaps = if attention {
            "AttentionIconPixmap"
        } else {
            "IconPixmap"
        };
        let name = if attention {
            "AttentionIconName"
        } else {
            "IconName"
        };
        let icon = props
            .get(pixmaps)
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| Vec::<(i32, i32, Vec<u8>)>::try_from(value).ok())
            .and_then(decode_pixmaps)
            .or_else(|| load_icon(&text(&props, name), &text(&props, "IconThemePath")))
            .or_else(|| {
                props
                    .get("IconPixmap")
                    .and_then(|value| value.try_clone().ok())
                    .and_then(|value| Vec::<(i32, i32, Vec<u8>)>::try_from(value).ok())
                    .and_then(decode_pixmaps)
            });
        let menu = props
            .get("Menu")
            .and_then(|value| <&zbus::zvariant::ObjectPath>::try_from(value).ok())
            .map(|path| path.to_string())
            .filter(|path| path != "/" && path != "/NO_DBUSMENU");
        let title = text(&props, "Title");
        return Some(Item {
            id: id.into(),
            title: if title.is_empty() {
                text(&props, "Id")
            } else {
                title
            },
            icon,
            menu,
            menu_only: props
                .get("ItemIsMenu")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false),
            interface: interface.into(),
        });
    }
    None
}
fn text(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or_default()
        .into()
}
fn decode_pixmaps(pixmaps: Vec<(i32, i32, Vec<u8>)>) -> Option<Icon> {
    let (w, h, bytes) = pixmaps
        .into_iter()
        .filter(|(w, h, bytes)| {
            *w > 0
                && *h > 0
                && *w <= 512
                && *h <= 512
                && bytes.len() == *w as usize * *h as usize * 4
        })
        .min_by_key(|(w, h, _)| {
            (
                if *w >= 22 && *h >= 22 { 0 } else { 1 },
                (*w - 22).abs() + (*h - 22).abs(),
            )
        })?;
    let rgba = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[1], p[2], p[3], p[0]])
        .collect();
    Some(Icon {
        size: [w as usize, h as usize],
        rgba,
    })
}
fn load_icon(name: &str, theme: &str) -> Option<Icon> {
    if name.is_empty() {
        return None;
    }
    let path = std::path::Path::new(name);
    if path.is_absolute() {
        return load_file(path);
    }
    if name.contains('/') || name.contains("..") {
        return None;
    }
    let mut roots = Vec::new();
    if !theme.is_empty() {
        roots.push(std::path::PathBuf::from(theme));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(std::path::PathBuf::from(home).join(".icons"));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(std::path::PathBuf::from(data).join("icons"));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(std::path::PathBuf::from(home).join(".local/share/icons"));
    }
    for root in std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".into())
        .split(':')
    {
        roots.push(std::path::PathBuf::from(root).join("icons"));
        roots.push(std::path::PathBuf::from(root).join("pixmaps"));
    }
    for root in roots {
        for theme in ["", "hicolor", "Adwaita"] {
            for size in [
                "", "16x16", "22x22", "24x24", "32x32", "48x48", "scalable", "symbolic",
            ] {
                for category in ["", "status", "apps", "devices", "actions", "panel"] {
                    for extension in ["png", "svg"] {
                        let path = root
                            .join(theme)
                            .join(size)
                            .join(category)
                            .join(format!("{name}.{extension}"));
                        if path.is_file()
                            && let Some(icon) = load_file(&path)
                        {
                            return Some(icon);
                        }
                    }
                }
            }
        }
    }
    None
}
fn load_file(path: &std::path::Path) -> Option<Icon> {
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if path.extension().is_some_and(|e| e == "svg") {
        let tree = resvg::usvg::Tree::from_data(&bytes, &resvg::usvg::Options::default()).ok()?;
        let mut pixmap = tiny_skia::Pixmap::new(32, 32)?;
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(32. / tree.size().width(), 32. / tree.size().height()),
            &mut pixmap.as_mut(),
        );
        let rgba = pixmap
            .pixels()
            .iter()
            .flat_map(|pixel| {
                let p = pixel.demultiply();
                [p.red(), p.green(), p.blue(), p.alpha()]
            })
            .collect();
        Some(Icon {
            size: [32, 32],
            rgba,
        })
    } else {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(512);
        limits.max_image_height = Some(512);
        limits.max_alloc = Some(4 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().ok()?.thumbnail(32, 32).into_rgba8();
        Some(Icon {
            size: [image.width() as usize, image.height() as usize],
            rgba: image.into_raw(),
        })
    }
}
fn read_menu(connection: &Connection, item: &Item, opening: bool) -> anyhow::Result<Vec<MenuRow>> {
    let (destination, _) = split_id(&item.id);
    let proxy = Proxy::new(
        connection,
        destination,
        item.menu
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("No application menu"))?,
        "com.canonical.dbusmenu",
    )?;
    if opening {
        let _: bool = bounded(proxy.inner().call("AboutToShow", &(0i32,)))?;
    }
    type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
    let (_, layout): (u32, Layout) = bounded(
        proxy
            .inner()
            .call("GetLayout", &(0i32, 8i32, Vec::<&str>::new())),
    )?;
    Ok(layout
        .2
        .into_iter()
        .filter_map(|v| parse_row(v, 0))
        .collect())
}
fn parse_row(value: OwnedValue, depth: usize) -> Option<MenuRow> {
    if depth > 8 {
        return None;
    }
    let (id, props, children) =
        match <(i32, HashMap<String, OwnedValue>, Vec<OwnedValue>)>::try_from(value) {
            Ok(row) => row,
            Err(error) => {
                eprintln!("Tray menu decoding: {error}");
                return None;
            }
        };
    let label = text(&props, "label")
        .replace("__", "\0")
        .replace('_', "")
        .replace('\0', "_");
    Some(MenuRow {
        id,
        label,
        enabled: props
            .get("enabled")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(true),
        visible: props
            .get("visible")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(true),
        separator: text(&props, "type") == "separator",
        checked: (!text(&props, "toggle-type").is_empty()).then(|| {
            props
                .get("toggle-state")
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0)
                == 1
        }),
        children: children
            .into_iter()
            .take(128)
            .filter_map(|v| parse_row(v, depth + 1))
            .collect(),
    })
}
fn action(connection: &Connection, items: &[Item], request: Request) -> anyhow::Result<()> {
    let id = match &request {
        Request::Activate(id, ..) | Request::Scroll(id, ..) | Request::Click(id, ..) => id,
        Request::OpenMenu(_) => return Ok(()),
    };
    let item = items
        .iter()
        .find(|item| &item.id == id)
        .ok_or_else(|| anyhow::anyhow!("Application closed"))?;
    let (destination, path) = split_id(id);
    let proxy = Proxy::new(connection, destination, path, item.interface.as_str())?;
    match request {
        Request::Activate(_, method, x, y) => {
            let _: () = bounded(proxy.inner().call(method, &(x, y)))?;
        }
        Request::Scroll(_, delta, orientation) => {
            let _: () = bounded(proxy.inner().call("Scroll", &(delta, orientation)))?;
        }
        Request::Click(_, row) => {
            let menu = Proxy::new(
                connection,
                destination,
                item.menu
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Application menu closed"))?,
                "com.canonical.dbusmenu",
            )?;
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u32;
            let _: () = bounded(
                menu.inner()
                    .call("Event", &(row, "clicked", Value::from(0i32), timestamp)),
            )?;
        }
        Request::OpenMenu(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dbusmenu_variant_children_decode() {
        let props: HashMap<String, OwnedValue> = HashMap::from([
            ("label".into(), zbus::zvariant::Str::from("_Open").into()),
            ("enabled".into(), false.into()),
        ]);
        let value = OwnedValue::try_from(Value::from((1i32, props, Vec::<Value>::new()))).unwrap();
        let row = parse_row(value, 0).expect("Menu row");
        assert_eq!(row.label, "Open");
        assert!(!row.enabled);
    }
    #[test]
    fn pixmaps_are_validated_and_argb_is_converted() {
        assert!(
            decode_pixmaps(vec![
                (i32::MAX, 2, vec![]),
                (0, 0, vec![]),
                (1, 1, vec![0; 3])
            ])
            .is_none()
        );
        let icon = decode_pixmaps(vec![(1, 1, vec![128, 10, 20, 30])]).unwrap();
        assert_eq!(icon.rgba, [10, 20, 30, 128]);
    }
    #[test]
    fn identifiers_accept_service_and_object_path() {
        assert_eq!(
            split_id("org.example.App"),
            ("org.example.App", "/StatusNotifierItem")
        );
        assert_eq!(split_id(":1.23/Tray/Item"), (":1.23", "/Tray/Item"));
    }
}
