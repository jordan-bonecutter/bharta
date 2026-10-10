//! Read kernel device identities without opening input or hidraw devices.
use super::{Device, Receiver, Snapshot};
use std::{collections::HashMap, path::Path};

pub(super) fn read() -> Snapshot {
    let fixture = (std::env::var("BHARTA_HEADLESS").as_deref() == Ok("1"))
        .then(|| std::env::var_os("BHARTA_TEST_SYSFS"))
        .flatten();
    read_at(
        fixture
            .as_deref()
            .map(Path::new)
            .unwrap_or(Path::new("/sys")),
    )
}
fn text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
fn entries(path: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
}
fn read_at(root: &Path) -> Snapshot {
    let mut snapshot = Snapshot::default();
    for path in entries(&root.join("class/bluetooth")) {
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("hci"))
        {
            // sysfs reports presence; Powered belongs to BlueZ, so never enable
            // discovery based only on a controller directory.
            snapshot.adapters.push((path.display().to_string(), false));
        }
    }
    for path in entries(&root.join("bus/hid/devices")) {
        let event = text(&path.join("uevent"));
        let props: HashMap<_, _> = event.lines().filter_map(|l| l.split_once('=')).collect();
        if !props
            .get("HID_ID")
            .is_some_and(|id| id.starts_with("0005:"))
        {
            continue;
        }
        let address = props
            .get("HID_UNIQ")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| path.display().to_string());
        let name = props
            .get("HID_NAME")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| address.clone());
        if snapshot
            .devices
            .iter()
            .any(|d| d.address.eq_ignore_ascii_case(&address))
        {
            continue;
        }
        snapshot.devices.push(Device {
            path: path.display().to_string(),
            name,
            address,
            connected: true,
            paired: false,
            rssi: None,
        });
    }
    for path in entries(&root.join("bus/usb/devices")) {
        let vendor = text(&path.join("idVendor"));
        let product_id = text(&path.join("idProduct"));
        // Interfaces have no USB product identity. Only list the physical
        // receiver once, even when it exports mouse, keyboard and HID++ interfaces.
        if vendor.len() != 4 || product_id.len() != 4 {
            continue;
        }
        let product = text(&path.join("product"));
        let lower = product.to_lowercase();
        if !(lower.contains("receiver") || lower.contains("dongle")) || lower.contains("bluetooth")
        {
            continue;
        }
        let manufacturer = text(&path.join("manufacturer"));
        let name = if manufacturer.is_empty() || lower.contains(&manufacturer.to_lowercase()) {
            product
        } else {
            format!("{manufacturer} {product}")
        };
        snapshot.receivers.push(Receiver {
            path: path.display().to_string(),
            name,
            id: format!("{vendor}:{product_id}"),
        });
    }
    snapshot.adapters.sort_by(|a, b| a.0.cmp(&b.0));
    snapshot
        .devices
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    snapshot
        .receivers
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, path: &str, value: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value).unwrap();
    }
    #[test]
    fn distinguishes_receiver_presence_bluetooth_inputs_and_wired_devices() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        for (path, value) in [
            ("bus/usb/devices/3-4/idVendor", "046d\n"),
            ("bus/usb/devices/3-4/idProduct", "c548\n"),
            ("bus/usb/devices/3-4/manufacturer", "Logitech\n"),
            ("bus/usb/devices/3-4/product", "USB Receiver\n"),
            ("bus/usb/devices/3-4:1.0/product", "USB Receiver"),
            ("bus/usb/devices/3-5/idVendor", "1234"),
            ("bus/usb/devices/3-5/idProduct", "5678"),
            ("bus/usb/devices/3-5/product", "Wired Mouse"),
            (
                "bus/hid/devices/bt1/uevent",
                "HID_ID=0005:0000046D:0000B023\nHID_NAME=Bluetooth mouse\nHID_UNIQ=AA:BB:CC:DD:EE:FF\n",
            ),
            (
                "bus/hid/devices/bt2/uevent",
                "HID_ID=0005:0000046D:0000B023\nHID_NAME=Bluetooth mouse\nHID_UNIQ=aa:bb:cc:dd:ee:ff\n",
            ),
            (
                "bus/hid/devices/usb/uevent",
                "HID_ID=0003:0000046D:0000C548\nHID_NAME=Logitech USB Receiver\n",
            ),
        ] {
            write(root, path, value);
        }
        std::fs::create_dir_all(root.join("class/bluetooth/hci0")).unwrap();
        let s = read_at(root);
        assert_eq!(s.receivers.len(), 1);
        assert_eq!(s.receivers[0].name, "Logitech USB Receiver");
        assert_eq!(s.receivers[0].id, "046d:c548");
        assert_eq!(s.devices.len(), 1);
        assert_eq!(s.devices[0].name, "Bluetooth mouse");
        assert_eq!(s.adapters.len(), 1);
        assert!(!s.enabled());
        std::fs::remove_dir_all(root.join("bus/usb/devices/3-4")).unwrap();
        assert!(read_at(root).receivers.is_empty());
    }
    #[test]
    fn missing_sysfs_is_an_empty_snapshot() {
        assert!(
            read_at(Path::new("/nonexistent/bharta/sysfs"))
                .receivers
                .is_empty()
        );
    }
}
