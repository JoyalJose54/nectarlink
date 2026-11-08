// SPDX-License-Identifier: GPL-3.0-or-later
//! Paired Bluetooth device inspection and Hands-Free Profile (HFP) call audio
//! routing on Linux via BlueZ (`org.bluez` D-Bus `ObjectManager` with
//! `bluetoothctl` fallback) and PipeWire/PulseAudio (`pactl set-card-profile`).

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const UUID_HFP_AG: &str = "0000111f-0000-1000-8000-00805f9b34fb";
const UUID_HFP_HF: &str = "0000111e-0000-1000-8000-00805f9b34fb";
const UUID_A2DP_SOURCE: &str = "0000110a-0000-1000-8000-00805f9b34fb";

type ManagedObjectsMap = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BluezEndpoint {
    pub object_path: String,
    pub mac: String,
    pub name: String,
    pub connected: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HfpStatus {
    pub available: bool,
    pub connected: bool,
    pub device_name: String,
    pub detail: String,
}

static ACTIVE_MAC: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();

fn active_slot() -> &'static Mutex<Option<(String, String)>> {
    ACTIVE_MAC.get_or_init(|| Mutex::new(None))
}

pub(crate) fn is_valid_bt_mac(mac: &str) -> bool {
    !mac.is_empty()
        && !mac.starts_with('-')
        && mac.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'-')
}

/// Parses `bluetoothctl devices` / `bluetoothctl devices Paired` output lines
/// of the form `"Device AA:BB:CC:DD:EE:FF Pixel 9 Pro"` into `(mac, name)` pairs.
pub(crate) fn parse_bluetoothctl_devices(output: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw_line in output.lines() {
        let line = raw_line.trim();
        let Some(rest) = line.strip_prefix("Device ") else {
            continue;
        };
        let rest = rest.trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let Some(mac) = parts.next().map(str::trim).filter(|m| is_valid_bt_mac(m)) else {
            continue;
        };
        let name = parts.next().map(str::trim).unwrap_or_default().to_owned();
        out.push((mac.to_owned(), name));
    }
    out
}

/// Parses `bluetoothctl info <mac>` output to determine whether `Connected: yes`
/// is present.
pub(crate) fn parse_bluetoothctl_connected(info_output: &str) -> bool {
    info_output.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.strip_prefix("Connected:").map(|v| v.trim().eq_ignore_ascii_case("yes")).unwrap_or(false)
    })
}

/// Formats a Bluetooth MAC address (`AA:BB:CC:DD:EE:FF`) into its PipeWire /
/// PulseAudio BlueZ card name (`bluez_card.AA_BB_CC_DD_EE_FF`).
pub(crate) fn bluez_card_name_for_mac(mac: &str) -> String {
    let underscored: String =
        mac.trim().chars().map(|c| if c == ':' || c == '-' { '_' } else { c }).collect();
    format!("bluez_card.{underscored}")
}

/// Selects the paired Bluetooth `(mac, name)` endpoint whose name matches
/// `phone_name` (case-insensitive substring match in either direction).
/// Never falls back to an unrelated device (e.g. a Bluetooth mouse or headset).
pub fn match_endpoint<'a>(
    endpoints: &'a [(String, String)],
    phone_name: Option<&str>,
) -> Option<&'a (String, String)> {
    if endpoints.is_empty() {
        return None;
    }
    let needle = phone_name.map(str::trim).filter(|s| !s.is_empty())?;
    let needle_lower = needle.to_ascii_lowercase();
    endpoints.iter().find(|(_, name)| {
        let hay = name.trim().to_ascii_lowercase();
        !hay.is_empty() && (hay.contains(&needle_lower) || needle_lower.contains(&hay))
    })
}

pub(crate) fn is_phone_capable_bluez_device(paired: bool, uuids: &[String]) -> bool {
    if !paired {
        return false;
    }
    if uuids.is_empty() {
        return true;
    }
    uuids.iter().any(|u| {
        let lower = u.trim().to_ascii_lowercase();
        lower == UUID_HFP_AG || lower == UUID_HFP_HF || lower == UUID_A2DP_SOURCE
    })
}

pub(crate) fn parse_bluez_managed_objects(objects: &ManagedObjectsMap) -> Vec<BluezEndpoint> {
    let mut out = Vec::new();
    for (path, ifaces) in objects {
        let Some(dev) = ifaces.get("org.bluez.Device1") else {
            continue;
        };
        let paired = dev.get("Paired").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        let uuids: Vec<String> = dev
            .get("UUIDs")
            .and_then(|v| v.try_clone().ok())
            .and_then(|owned| Vec::<String>::try_from(owned).ok())
            .unwrap_or_default();
        if !is_phone_capable_bluez_device(paired, &uuids) {
            continue;
        }
        let Some(mac) = dev
            .get("Address")
            .and_then(|v| <&str>::try_from(v).ok())
            .map(str::trim)
            .filter(|m| is_valid_bt_mac(m))
        else {
            continue;
        };
        let name = dev
            .get("Alias")
            .and_then(|v| <&str>::try_from(v).ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                dev.get("Name")
                    .and_then(|v| <&str>::try_from(v).ok())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default()
            .to_owned();
        let connected = dev.get("Connected").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        out.push(BluezEndpoint {
            object_path: path.as_str().to_owned(),
            mac: mac.to_owned(),
            name,
            connected,
        });
    }
    out.sort_by(|a, b| a.mac.cmp(&b.mac));
    out
}

fn enumerate_bluez_dbus_devices() -> Option<Vec<BluezEndpoint>> {
    super::non_tokio(|| {
        let conn = zbus::blocking::Connection::system().ok()?;
        if !super::dbus_name_has_owner(&conn, "org.bluez") {
            return None;
        }
        let reply = conn
            .call_method(
                Some("org.bluez"),
                "/",
                Some("org.freedesktop.DBus.ObjectManager"),
                "GetManagedObjects",
                &(),
            )
            .ok()?;
        let body = reply.body();
        let objects = body
            .deserialize::<ManagedObjectsMap>()
            .ok()
            .or_else(|| body.deserialize::<(ManagedObjectsMap,)>().ok().map(|(m,)| m))?;
        Some(parse_bluez_managed_objects(&objects))
    })
}

fn enumerate_paired_devices() -> Vec<(String, String)> {
    if let Some(bluez) = enumerate_bluez_dbus_devices()
        && !bluez.is_empty()
    {
        return bluez.into_iter().map(|e| (e.mac, e.name)).collect();
    }
    if let Ok(out) = Command::new("bluetoothctl").args(["devices", "Paired"]).output()
        && out.status.success()
    {
        let parsed = parse_bluetoothctl_devices(&String::from_utf8_lossy(&out.stdout));
        if !parsed.is_empty() {
            return parsed;
        }
    }
    if let Ok(out) = Command::new("bluetoothctl").arg("devices").output()
        && out.status.success()
    {
        return parse_bluetoothctl_devices(&String::from_utf8_lossy(&out.stdout));
    }
    Vec::new()
}

fn query_device_connected(mac: &str) -> bool {
    if let Some(bluez) = enumerate_bluez_dbus_devices()
        && let Some(found) = bluez.iter().find(|e| e.mac.eq_ignore_ascii_case(mac))
    {
        return found.connected;
    }
    Command::new("bluetoothctl")
        .args(["info", mac])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_bluetoothctl_connected(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(false)
}

pub fn probe_status(phone_name: Option<&str>) -> HfpStatus {
    let endpoints = enumerate_paired_devices();
    if let Some((mac, name)) = match_endpoint(&endpoints, phone_name) {
        let display = if name.trim().is_empty() {
            phone_name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(mac.as_str()).to_owned()
        } else {
            name.clone()
        };
        let connected = query_device_connected(mac);
        let detail = if connected {
            format!("Bluetooth connected ({display}).")
        } else {
            format!("Bluetooth paired with {display} (not currently connected).")
        };
        return HfpStatus { available: true, connected, device_name: display, detail };
    }

    HfpStatus {
        available: false,
        connected: false,
        device_name: phone_name.unwrap_or_default().to_string(),
        detail: "Pair your phone over system Bluetooth to route call audio through this PC.".to_string(),
    }
}

pub fn connect_audio(phone_name: Option<&str>) -> Result<HfpStatus, String> {
    let bluez_devices = enumerate_bluez_dbus_devices().unwrap_or_default();
    let endpoints: Vec<(String, String)> = if !bluez_devices.is_empty() {
        bluez_devices.iter().map(|e| (e.mac.clone(), e.name.clone())).collect()
    } else {
        enumerate_paired_devices()
    };
    let Some((mac, name)) = match_endpoint(&endpoints, phone_name) else {
        return Err("No paired Bluetooth phone endpoint found on this PC.".to_string());
    };
    let display = if name.trim().is_empty() {
        phone_name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(mac.as_str()).to_owned()
    } else {
        name.clone()
    };

    let connected_via_dbus = bluez_devices
        .iter()
        .find(|e| e.mac.eq_ignore_ascii_case(mac))
        .and_then(|e| {
            super::non_tokio(|| {
                let conn = zbus::blocking::Connection::system().ok()?;
                if !super::dbus_name_has_owner(&conn, "org.bluez") {
                    return None;
                }
                conn.call_method(
                    Some("org.bluez"),
                    e.object_path.as_str(),
                    Some("org.bluez.Device1"),
                    "Connect",
                    &(),
                )
                .ok()
            })
        })
        .is_some();
    if !connected_via_dbus {
        let _ = Command::new("bluetoothctl").args(["connect", mac]).status();
    }

    let card = bluez_card_name_for_mac(mac);
    for profile in [
        "headset-audio-gateway",
        "handsfree_head_unit",
        "headset_head_unit",
        "headset-head-unit",
        "headset-head-unit-cvsd",
        "hfp_hf",
        "a2dp-sink",
    ] {
        if Command::new("pactl")
            .args(["set-card-profile", &card, profile])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            break;
        }
    }

    let connected = query_device_connected(mac);
    if let Ok(mut guard) = active_slot().lock() {
        *guard = Some((mac.clone(), display.clone()));
    }
    let detail = if connected {
        format!("Bluetooth HFP call audio connected ({display}).")
    } else {
        format!("Bluetooth paired with {display} (not currently connected).")
    };
    Ok(HfpStatus { available: true, connected, device_name: display, detail })
}

pub fn disconnect_audio() {
    if let Ok(mut guard) = active_slot().lock()
        && let Some((mac, _)) = guard.take()
    {
        let card = bluez_card_name_for_mac(&mac);
        let _ = Command::new("pactl")
            .args(["set-card-profile", &card, "a2dp-sink"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub fn open_bluetooth_settings() {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_kde = desktop.contains("kde") || desktop.contains("plasma");

    if is_kde
        && (Command::new("systemsettings").arg("kcm_bluetooth").spawn().is_ok()
            || Command::new("kcmshell6").arg("kcm_bluetooth").spawn().is_ok())
    {
        return;
    }

    let _ = Command::new("gnome-control-center")
        .arg("bluetooth")
        .spawn()
        .or_else(|_| Command::new("systemsettings").arg("kcm_bluetooth").spawn())
        .or_else(|_| Command::new("kcmshell6").arg("kcm_bluetooth").spawn())
        .or_else(|_| Command::new("blueman-manager").spawn())
        .or_else(|_| Command::new("overskride").spawn())
        .or_else(|_| Command::new("xdg-open").arg("settings://bluetooth").spawn());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_endpoint_prefers_phone_name_case_insensitive() {
        let list = vec![
            ("id-1".to_string(), "Galaxy Buds".to_string()),
            ("id-2".to_string(), "Pixel 9 Pro".to_string()),
        ];
        let matched = match_endpoint(&list, Some("pixel 9")).unwrap();
        assert_eq!(matched.0, "id-2");
    }

    #[test]
    fn match_endpoint_returns_none_when_unmatched_or_missing_name() {
        let list = vec![("id-1".to_string(), "MX Master 3S".to_string())];
        assert!(match_endpoint(&list, Some("Pixel 9 Pro")).is_none());
        assert!(match_endpoint(&list, None).is_none());
        assert!(match_endpoint(&list, Some("   ")).is_none());
    }

    #[test]
    fn bluez_managed_objects_parser_filters_unpaired_and_non_audio_hid_devices() {
        use zbus::zvariant::{ObjectPath, Str, Value};

        let mut objects: ManagedObjectsMap = HashMap::new();

        // 1. Paired phone advertising HFP Audio Gateway
        let mut phone_props = HashMap::new();
        phone_props.insert(
            "Address".to_owned(),
            OwnedValue::try_from(Value::from(Str::from("AA:BB:CC:DD:EE:FF"))).unwrap(),
        );
        phone_props
            .insert("Alias".to_owned(), OwnedValue::try_from(Value::from(Str::from("Pixel 9 Pro"))).unwrap());
        phone_props.insert("Paired".to_owned(), OwnedValue::from(true));
        phone_props.insert("Connected".to_owned(), OwnedValue::from(true));
        let phone_uuids = vec![Str::from(UUID_HFP_AG)];
        phone_props.insert("UUIDs".to_owned(), OwnedValue::try_from(Value::from(phone_uuids)).unwrap());
        let mut phone_ifaces = HashMap::new();
        phone_ifaces.insert("org.bluez.Device1".to_owned(), phone_props);
        objects.insert(
            OwnedObjectPath::from(ObjectPath::from_static_str_unchecked(
                "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
            )),
            phone_ifaces,
        );

        // 2. Paired BLE mouse advertising only HID over GATT (00001812-...)
        let mut mouse_props = HashMap::new();
        mouse_props.insert(
            "Address".to_owned(),
            OwnedValue::try_from(Value::from(Str::from("11:22:33:44:55:66"))).unwrap(),
        );
        mouse_props.insert(
            "Alias".to_owned(),
            OwnedValue::try_from(Value::from(Str::from("MX Master 3S"))).unwrap(),
        );
        mouse_props.insert("Paired".to_owned(), OwnedValue::from(true));
        mouse_props.insert("Connected".to_owned(), OwnedValue::from(true));
        let mouse_uuids = vec![Str::from("00001812-0000-1000-8000-00805f9b34fb")];
        mouse_props.insert("UUIDs".to_owned(), OwnedValue::try_from(Value::from(mouse_uuids)).unwrap());
        let mut mouse_ifaces = HashMap::new();
        mouse_ifaces.insert("org.bluez.Device1".to_owned(), mouse_props);
        objects.insert(
            OwnedObjectPath::from(ObjectPath::from_static_str_unchecked(
                "/org/bluez/hci0/dev_11_22_33_44_55_66",
            )),
            mouse_ifaces,
        );

        let parsed = parse_bluez_managed_objects(&objects);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(parsed[0].name, "Pixel 9 Pro");
        assert!(parsed[0].connected);
    }

    #[test]
    fn parse_bluetoothctl_devices_extracts_mac_and_name() {
        let sample = "\
Device 11:22:33:44:55:66 Galaxy S24 Ultra
Device AA:BB:CC:DD:EE:FF Pixel 9 Pro
Invalid line
";
        let list = parse_bluetoothctl_devices(sample);
        assert_eq!(
            list,
            vec![
                ("11:22:33:44:55:66".to_string(), "Galaxy S24 Ultra".to_string()),
                ("AA:BB:CC:DD:EE:FF".to_string(), "Pixel 9 Pro".to_string()),
            ]
        );
    }

    #[test]
    fn parse_bluetoothctl_connected_detects_connection_state() {
        let connected_info = "\
Device AA:BB:CC:DD:EE:FF (public)
\tName: Pixel 9 Pro
\tPaired: yes
\tConnected: yes
";
        let disconnected_info = "\
Device AA:BB:CC:DD:EE:FF (public)
\tPaired: yes
\tConnected: no
";
        assert!(parse_bluetoothctl_connected(connected_info));
        assert!(!parse_bluetoothctl_connected(disconnected_info));
    }

    #[test]
    fn bluez_card_name_for_mac_replaces_colons_with_underscores() {
        assert_eq!(bluez_card_name_for_mac("AA:BB:CC:DD:EE:FF"), "bluez_card.AA_BB_CC_DD_EE_FF");
    }
}
