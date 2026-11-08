// SPDX-License-Identifier: GPL-3.0-or-later
//! Bluetooth through BlueZ over D-Bus. Broadcasts the PC's proximity beacon
//! (company `0xFFFF`, `NCTR\x01` + 8-byte device ID prefix, as on Windows) so
//! phones set to "Wake when PC is nearby" find it, and, when walk-away
//! automation is on, watches the signal strength of paired phones. Never
//! spawns `bluetoothctl` or starts a scan of its own.

use std::collections::HashMap;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use nectarlink_core::DeviceId;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

pub const COMPANY_ID: u16 = 0xFFFF;
pub const MAGIC: &[u8; 5] = b"NCTR\x01";
pub const NEAR_RSSI_DBM: i16 = -72;
pub const WALK_AWAY_RSSI_DBM: i16 = -85;
pub const NECTARLINK_BLE_UUID: &str = "f81d4fae-7dec-11d0-a765-00a0c91e6bf6";

const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often the beacon is checked when walk-away is off: it comes back this
/// long after Bluetooth is turned back on.
const BEACON_CHECK_INTERVAL: Duration = Duration::from_secs(15);

const BLUEZ: &str = "org.bluez";
const ADVERTISING_MANAGER: &str = "org.bluez.LEAdvertisingManager1";
const ADVERTISEMENT_PATH: &str = "/app/nectarlink/beacon";

static RUNNING: AtomicBool = AtomicBool::new(false);
static STOP_MU: Mutex<bool> = Mutex::new(false);
static STOP_CV: Condvar = Condvar::new();
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedPhone {
    pub prefix: [u8; 8],
    pub name: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BluezDeviceCandidate {
    pub address: Option<String>,
    pub name: Option<String>,
    pub alias: Option<String>,
    pub rssi: Option<i16>,
    pub uuids: Vec<String>,
    pub mfg_payload: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct RssiTracker {
    peers: HashMap<[u8; 8], PeerRssiState>,
}

#[derive(Debug, Clone, Default)]
struct PeerRssiState {
    samples: [i16; 4],
    count: usize,
    idx: usize,
    armed_near: bool,
}

impl RssiTracker {
    pub fn record(&mut self, prefix: [u8; 8], rssi: i16) -> bool {
        let state = self.peers.entry(prefix).or_default();
        state.samples[state.idx] = rssi;
        state.idx = (state.idx + 1) % state.samples.len();
        state.count = (state.count + 1).min(state.samples.len());
        if state.count < state.samples.len() {
            return false;
        }
        let sum: i32 = state.samples.iter().map(|&v| i32::from(v)).sum();
        let avg = (sum / (state.samples.len() as i32)) as i16;
        if avg >= NEAR_RSSI_DBM {
            state.armed_near = true;
            false
        } else if state.armed_near && avg <= WALK_AWAY_RSSI_DBM {
            state.armed_near = false;
            true
        } else {
            false
        }
    }
}

pub fn build_manufacturer_payload(device_id: &DeviceId) -> [u8; 13] {
    let mut out = [0u8; 13];
    out[0..5].copy_from_slice(MAGIC);
    out[5..13].copy_from_slice(&device_id.as_bytes()[0..8]);
    out
}

/// Validates the `NCTR\x01` magic header and extracts the peer device ID prefix.
/// Accepts either the 13-byte payload (5-byte magic + 8-byte prefix) or a 12-byte
/// payload (5-byte magic + 7-byte prefix, zero-padded in the 8th byte).
pub fn parse_manufacturer_payload(bytes: &[u8]) -> Option<[u8; 8]> {
    if bytes.len() < 12 || &bytes[0..5] != MAGIC {
        return None;
    }
    let mut prefix = [0u8; 8];
    if bytes.len() >= 13 {
        prefix.copy_from_slice(&bytes[5..13]);
    } else {
        prefix[0..7].copy_from_slice(&bytes[5..12]);
    }
    Some(prefix)
}

pub fn start(device_id: DeviceId) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    *STOP_MU.lock().unwrap_or_else(|e| e.into_inner()) = false;
    let handle = thread::Builder::new().name("ble-beacon".into()).spawn(move || run_worker(device_id)).ok();
    if let Ok(mut guard) = WORKER.lock() {
        *guard = handle;
    }
}

pub fn stop() {
    RUNNING.store(false, Ordering::SeqCst);
    {
        let mut stopped = STOP_MU.lock().unwrap_or_else(|e| e.into_inner());
        *stopped = true;
        STOP_CV.notify_all();
    }
    let handle = WORKER.lock().ok().and_then(|mut guard| guard.take());
    if let Some(h) = handle {
        let _ = h.join();
    }
}

fn walk_away_lock_enabled() -> bool {
    let rules = crate::settings::automation_rules();
    !crate::automations::matching_rules(&rules, crate::automations::TRIGGER_PHONE_WALKED_AWAY).is_empty()
}

fn paired_phones_from_hub() -> Vec<PairedPhone> {
    crate::core_host::host().hub.read(|s| {
        s.devices
            .iter()
            .map(|d| {
                let mut prefix = [0u8; 8];
                prefix.copy_from_slice(&d.id.as_bytes()[0..8]);
                PairedPhone { prefix, name: d.info.name.clone(), model: d.info.model.clone() }
            })
            .collect()
    })
}

fn fallback_peer_prefix(id_str: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    let bytes = id_str.as_bytes();
    let start = bytes.len().saturating_sub(8);
    for (dst, &src) in out.iter_mut().zip(&bytes[start..]) {
        *dst = src;
    }
    out
}

fn names_match(candidate: &str, phone_name: &str) -> bool {
    let c = candidate.trim();
    let p = phone_name.trim();
    !c.is_empty() && !p.is_empty() && c.eq_ignore_ascii_case(p)
}

/// Matches a BlueZ device candidate ONLY if:
/// 1. It carries a valid `NCTR\x01` manufacturer payload (not our own prefix), OR
/// 2. It advertises the NectarLink BLE service UUID (`f81d4fae-7dec-11d0-a765-00a0c91e6bf6`), OR
/// 3. Its `Name` or `Alias` matches a paired phone in `hub`.
///
/// Arbitrary Bluetooth peripherals (headphones, mice, keyboards, etc.) never match.
pub fn match_nectarlink_device(
    candidate: &BluezDeviceCandidate,
    own_prefix: &[u8; 8],
    paired_phones: &[PairedPhone],
) -> Option<[u8; 8]> {
    if let Some(mfg_bytes) = &candidate.mfg_payload
        && let Some(prefix) = parse_manufacturer_payload(mfg_bytes)
    {
        if &prefix == own_prefix {
            return None;
        }
        if paired_phones.is_empty() || paired_phones.iter().any(|p| p.prefix == prefix) {
            return Some(prefix);
        }
    }

    let find_matching_phone = || -> Option<&PairedPhone> {
        paired_phones.iter().find(|phone| {
            candidate.name.as_deref().is_some_and(|n| {
                names_match(n, &phone.name) || phone.model.as_deref().is_some_and(|m| names_match(n, m))
            }) || candidate.alias.as_deref().is_some_and(|a| {
                names_match(a, &phone.name) || phone.model.as_deref().is_some_and(|m| names_match(a, m))
            })
        })
    };

    if candidate.uuids.iter().any(|u| u.eq_ignore_ascii_case(NECTARLINK_BLE_UUID)) {
        if let Some(phone) = find_matching_phone() {
            return Some(phone.prefix);
        }
        if let Some(first) = paired_phones.first() {
            return Some(first.prefix);
        }
        if let Some(addr) = &candidate.address {
            let prefix = fallback_peer_prefix(addr);
            if &prefix != own_prefix {
                return Some(prefix);
            }
        }
        return None;
    }

    find_matching_phone().map(|p| p.prefix)
}

fn extract_string(val: &OwnedValue) -> Option<String> {
    <&str>::try_from(val).ok().map(str::to_owned)
}

fn extract_i16(val: &OwnedValue) -> Option<i16> {
    if let Ok(v) = i16::try_from(val) {
        return Some(v);
    }
    if let Ok(v) = i32::try_from(val) {
        return i16::try_from(v).ok();
    }
    None
}

fn extract_string_list(val: &OwnedValue) -> Vec<String> {
    if let Ok(list) = Vec::<String>::try_from(val.clone()) {
        return list;
    }
    let Value::Array(arr) = &**val else {
        return Vec::new();
    };
    arr.iter().filter_map(|item| <&str>::try_from(item).ok().map(str::to_owned)).collect()
}

fn extract_mfg_ffff(val: &OwnedValue) -> Option<Vec<u8>> {
    let Value::Dict(dict) = &**val else {
        return None;
    };
    for (k, v) in dict.iter() {
        let key_matches = u16::try_from(k).is_ok_and(|id| id == COMPANY_ID);
        if !key_matches {
            continue;
        }
        let inner = match v {
            Value::Value(inner) => inner.as_ref(),
            other => other,
        };
        if let Value::Array(bytes_arr) = inner {
            let mut out = Vec::with_capacity(bytes_arr.len());
            for item in bytes_arr.iter() {
                out.push(u8::try_from(item).ok()?);
            }
            return Some(out);
        }
    }
    None
}

/// The beacon as BlueZ reads it: a non-connectable broadcast that carries only
/// the manufacturer data.
struct Advertisement {
    payload: [u8; 13],
    released: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    /// BlueZ dropped the beacon (its adapter went away); it is registered again.
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
    }

    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "broadcast".to_owned()
    }

    #[zbus(property)]
    fn manufacturer_data(&self) -> HashMap<u16, OwnedValue> {
        let bytes =
            OwnedValue::try_from(Value::from(self.payload.to_vec())).expect("byte arrays hold no fds");
        HashMap::from([(COMPANY_ID, bytes)])
    }
}

/// Keeps the beacon registered with the first powered adapter that can
/// advertise, following adapters as they are turned off and on.
struct Beacon {
    adapter: Option<OwnedObjectPath>,
    released: Arc<AtomicBool>,
    last_error: Option<String>,
}

impl Beacon {
    /// Connects with the beacon served. It has to be served from the builder:
    /// adding it to a connection afterwards needs a tokio runtime.
    fn serve(
        builder: zbus::blocking::connection::Builder<'_>,
        device_id: &DeviceId,
    ) -> zbus::Result<(zbus::blocking::Connection, Self)> {
        let released = Arc::new(AtomicBool::new(false));
        let advertisement =
            Advertisement { payload: build_manufacturer_payload(device_id), released: released.clone() };
        let conn = builder.serve_at(ADVERTISEMENT_PATH, advertisement)?.build()?;
        Ok((conn, Self { adapter: None, released, last_error: None }))
    }

    fn sync(&mut self, conn: &zbus::blocking::Connection, objects: &ManagedObjects) {
        if self.released.swap(false, Ordering::SeqCst) {
            self.adapter = None;
        }
        let wanted = advertising_adapter(objects);
        if self.adapter.is_some() && self.adapter == wanted {
            return;
        }
        if let Some(previous) = self.adapter.take() {
            unregister(conn, &previous);
        }
        let Some(adapter) = wanted else {
            return;
        };
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        let path = ObjectPath::from_static_str_unchecked(ADVERTISEMENT_PATH);
        match conn.call_method(
            Some(BLUEZ),
            &adapter,
            Some(ADVERTISING_MANAGER),
            "RegisterAdvertisement",
            &(path, options),
        ) {
            Ok(_) => {
                tracing::info!(adapter = adapter.as_str(), "broadcasting the Bluetooth beacon");
                self.adapter = Some(adapter);
                self.last_error = None;
            }
            Err(e) => {
                // Tried again on every check, so say it once.
                let error = e.to_string();
                if self.last_error.as_ref() != Some(&error) {
                    tracing::warn!(%error, "this PC can't broadcast its Bluetooth beacon");
                    self.last_error = Some(error);
                }
            }
        }
    }

    fn stop(&mut self, conn: &zbus::blocking::Connection) {
        if let Some(adapter) = self.adapter.take() {
            unregister(conn, &adapter);
        }
    }
}

fn unregister(conn: &zbus::blocking::Connection, adapter: &OwnedObjectPath) {
    let path = ObjectPath::from_static_str_unchecked(ADVERTISEMENT_PATH);
    // Fails when the adapter is gone, which takes the beacon with it.
    let _ = conn.call_method(
        Some(BLUEZ),
        adapter,
        Some(ADVERTISING_MANAGER),
        "UnregisterAdvertisement",
        &(path,),
    );
}

/// The first powered adapter (by path, so the choice is stable) that can
/// advertise.
fn advertising_adapter(objects: &ManagedObjects) -> Option<OwnedObjectPath> {
    objects
        .iter()
        .filter(|(_, ifaces)| {
            ifaces.contains_key(ADVERTISING_MANAGER)
                && ifaces
                    .get("org.bluez.Adapter1")
                    .and_then(|props| props.get("Powered"))
                    .and_then(|v| bool::try_from(v).ok())
                    .unwrap_or(false)
        })
        .map(|(path, _)| path)
        .min_by(|a, b| a.as_str().cmp(b.as_str()))
        .cloned()
}

fn managed_objects(conn: &zbus::blocking::Connection) -> Option<ManagedObjects> {
    let reply = conn
        .call_method(Some(BLUEZ), "/", Some("org.freedesktop.DBus.ObjectManager"), "GetManagedObjects", &())
        .ok()?;
    reply.body().deserialize::<ManagedObjects>().ok()
}

fn poll_bluez_devices(objects: &ManagedObjects, own_prefix: &[u8; 8], tracker: &mut RssiTracker) {
    let paired_phones = paired_phones_from_hub();
    if paired_phones.is_empty() {
        return;
    }

    for ifaces in objects.values() {
        let Some(props) = ifaces.get("org.bluez.Device1") else {
            continue;
        };
        let Some(rssi) = props.get("RSSI").and_then(extract_i16) else {
            continue;
        };
        let candidate = BluezDeviceCandidate {
            address: props.get("Address").and_then(extract_string),
            name: props.get("Name").and_then(extract_string),
            alias: props.get("Alias").and_then(extract_string),
            rssi: Some(rssi),
            uuids: props.get("UUIDs").map(extract_string_list).unwrap_or_default(),
            mfg_payload: props.get("ManufacturerData").and_then(extract_mfg_ffff),
        };
        if let Some(prefix) = match_nectarlink_device(&candidate, own_prefix, &paired_phones)
            && tracker.record(prefix, rssi)
        {
            tracing::info!("BLE walk-away threshold reached for paired phone; firing trigger");
            crate::automations::fire_trigger(crate::automations::TRIGGER_PHONE_WALKED_AWAY);
        }
    }
}

fn run_worker(device_id: DeviceId) {
    let served = zbus::blocking::connection::Builder::system().and_then(|b| Beacon::serve(b, &device_id));
    let (conn, mut beacon) = match served {
        Ok((conn, beacon)) => (conn, Some(beacon)),
        Err(e) => {
            tracing::debug!(error = %e, "BLE beacon: can't serve it on the system D-Bus");
            match zbus::blocking::Connection::system() {
                Ok(conn) => (conn, None),
                Err(_) => {
                    tracing::debug!("BLE beacon: system D-Bus unavailable; exiting worker");
                    RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
            }
        }
    };
    if !super::dbus_name_has_owner(&conn, BLUEZ) {
        tracing::debug!("BLE beacon: BlueZ isn't running; exiting worker");
        RUNNING.store(false, Ordering::SeqCst);
        return;
    }

    let mut own_prefix = [0u8; 8];
    own_prefix.copy_from_slice(&device_id.as_bytes()[0..8]);
    let mut tracker = RssiTracker::default();

    while RUNNING.load(Ordering::Relaxed) {
        let walk_away = walk_away_lock_enabled();
        if let Some(objects) = managed_objects(&conn) {
            if let Some(beacon) = beacon.as_mut() {
                beacon.sync(&conn, &objects);
            }
            if walk_away {
                poll_bluez_devices(&objects, &own_prefix, &mut tracker);
            }
        }

        let interval = if walk_away { POLL_INTERVAL } else { BEACON_CHECK_INTERVAL };
        let guard = STOP_MU.lock().unwrap_or_else(|e| e.into_inner());
        let _ = STOP_CV
            .wait_timeout_while(guard, interval, |stopped| !*stopped && RUNNING.load(Ordering::Relaxed));
    }
    if let Some(beacon) = beacon.as_mut() {
        beacon.stop(&conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_13_byte_manufacturer_payload_with_magic_and_id_prefix() {
        let mut raw = [0u8; 32];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        let id = DeviceId(raw);
        let payload = build_manufacturer_payload(&id);
        assert_eq!(payload.len(), 13);
        assert_eq!(&payload[0..5], b"NCTR\x01");
        assert_eq!(&payload[5..13], &raw[0..8]);
        assert_eq!(COMPANY_ID, 0xFFFF);
    }

    #[test]
    fn manufacturer_payload_roundtrips_through_parser() {
        let mut raw = [0u8; 32];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(0x10);
        }
        let id = DeviceId(raw);
        let payload = build_manufacturer_payload(&id);
        let parsed = parse_manufacturer_payload(&payload).expect("valid payload should parse");
        assert_eq!(&parsed[..], &raw[0..8]);

        // Also supports 12-byte payloads (5-byte magic + 7-byte prefix).
        let parsed_12 = parse_manufacturer_payload(&payload[0..12]).expect("12-byte payload should parse");
        assert_eq!(&parsed_12[0..7], &raw[0..7]);
        assert_eq!(parsed_12[7], 0);
    }

    #[test]
    fn parser_rejects_short_or_wrong_magic_payloads() {
        assert_eq!(parse_manufacturer_payload(b""), None);
        assert_eq!(parse_manufacturer_payload(b"NCTR\x01short"), None);
        assert_eq!(parse_manufacturer_payload(b"NCTR\x0212345678"), None);
        assert_eq!(parse_manufacturer_payload(b"XXXX\x0112345678"), None);
    }

    #[test]
    fn rssi_tracker_fires_walk_away_only_after_armed_near() {
        let mut tracker = RssiTracker::default();
        let peer = [1, 2, 3, 4, 5, 6, 7, 8];
        for _ in 0..4 {
            assert!(!tracker.record(peer, -90));
        }
        for _ in 0..4 {
            assert!(!tracker.record(peer, -65));
        }
        assert!(!tracker.record(peer, -88));
        assert!(!tracker.record(peer, -90));
        assert!(!tracker.record(peer, -92));
        assert!(tracker.record(peer, -90));
        assert!(!tracker.record(peer, -92));
    }

    #[test]
    fn rssi_tracker_arms_at_near_threshold_and_fires_once_on_walk_away() {
        let mut tracker = RssiTracker::default();
        let peer = [9, 8, 7, 6, 5, 4, 3, 2];
        // Exactly at NEAR_RSSI_DBM (-72) for 4 samples arms the tracker.
        for _ in 0..4 {
            assert!(!tracker.record(peer, NEAR_RSSI_DBM));
        }
        // Dropping to WALK_AWAY_RSSI_DBM (-85) across 4 samples triggers once.
        assert!(!tracker.record(peer, WALK_AWAY_RSSI_DBM));
        assert!(!tracker.record(peer, WALK_AWAY_RSSI_DBM));
        assert!(!tracker.record(peer, WALK_AWAY_RSSI_DBM));
        assert!(tracker.record(peer, WALK_AWAY_RSSI_DBM));
        // Subsequent distant samples do not re-trigger without re-arming near.
        for _ in 0..4 {
            assert!(!tracker.record(peer, -95));
        }
    }

    #[test]
    fn matches_only_paired_phones_or_nectarlink_uuid_and_ignores_peripherals() {
        let own_prefix = [0xAA; 8];
        let phone_prefix = [1, 2, 3, 4, 5, 6, 7, 8];
        let paired = vec![PairedPhone {
            prefix: phone_prefix,
            name: "Pixel 9 Pro".into(),
            model: Some("Pixel 9 Pro".into()),
        }];

        // Arbitrary Bluetooth headphones / mice must NEVER match.
        let headphones = BluezDeviceCandidate {
            address: Some("11:22:33:44:55:66".into()),
            name: Some("Sony WH-1000XM5".into()),
            alias: Some("Sony WH-1000XM5".into()),
            rssi: Some(-90),
            uuids: vec!["0000110b-0000-1000-8000-00805f9b34fb".into()],
            mfg_payload: None,
        };
        assert_eq!(match_nectarlink_device(&headphones, &own_prefix, &paired), None);

        // Paired phone matched by Name/Alias.
        let by_name = BluezDeviceCandidate {
            address: Some("AA:BB:CC:DD:EE:FF".into()),
            name: Some("pixel 9 pro".into()),
            alias: None,
            rssi: Some(-65),
            uuids: Vec::new(),
            mfg_payload: None,
        };
        assert_eq!(match_nectarlink_device(&by_name, &own_prefix, &paired), Some(phone_prefix));

        // Device advertising the NectarLink BLE service UUID matches.
        let by_uuid = BluezDeviceCandidate {
            address: Some("AA:BB:CC:DD:EE:FF".into()),
            name: Some("Android".into()),
            alias: None,
            rssi: Some(-65),
            uuids: vec![NECTARLINK_BLE_UUID.to_uppercase()],
            mfg_payload: None,
        };
        assert_eq!(match_nectarlink_device(&by_uuid, &own_prefix, &paired), Some(phone_prefix));
    }

    fn adapter(path: &str, powered: bool, can_advertise: bool) -> ManagedObjects {
        let mut ifaces = HashMap::from([(
            "org.bluez.Adapter1".to_owned(),
            HashMap::from([("Powered".to_owned(), powered.into())]),
        )]);
        if can_advertise {
            ifaces.insert(ADVERTISING_MANAGER.to_owned(), HashMap::new());
        }
        HashMap::from([(OwnedObjectPath::try_from(path).unwrap(), ifaces)])
    }

    #[test]
    fn beacon_uses_the_first_powered_adapter_that_can_advertise() {
        let mut objects = adapter("/org/bluez/hci2", true, true);
        objects.extend(adapter("/org/bluez/hci0", false, true));
        objects.extend(adapter("/org/bluez/hci1", true, false));
        objects.extend(adapter("/org/bluez/hci3", true, true));
        assert_eq!(advertising_adapter(&objects).unwrap().as_str(), "/org/bluez/hci2");
        assert_eq!(advertising_adapter(&adapter("/org/bluez/hci0", false, true)), None);
        assert_eq!(advertising_adapter(&HashMap::new()), None);
    }

    /// A private bus from `dbus-daemon`, stopped when dropped.
    struct TestBus {
        daemon: std::process::Child,
        address: String,
    }

    impl TestBus {
        fn start() -> Option<Self> {
            use std::io::BufRead;
            let mut daemon = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address"])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .ok()?;
            let mut address = String::new();
            std::io::BufReader::new(daemon.stdout.take()?).read_line(&mut address).ok()?;
            Some(Self { daemon, address: address.trim().to_owned() })
        }

        fn connect(&self) -> zbus::blocking::connection::Builder<'static> {
            zbus::blocking::connection::Builder::address(self.address.as_str()).unwrap()
        }
    }

    impl Drop for TestBus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    /// Stands in for BlueZ's advertising manager: reads each beacon back the
    /// way BlueZ does and records what it was asked.
    #[derive(Clone, Default)]
    struct FakeManager {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl FakeManager {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }

    #[zbus::interface(name = "org.bluez.LEAdvertisingManager1")]
    impl FakeManager {
        async fn register_advertisement(
            &self,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] conn: &zbus::Connection,
            path: OwnedObjectPath,
            _options: HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<()> {
            let reply = conn
                .call_method(
                    header.sender().map(|s| s.as_str()),
                    &path,
                    Some("org.freedesktop.DBus.Properties"),
                    "GetAll",
                    &("org.bluez.LEAdvertisement1",),
                )
                .await?;
            let props: HashMap<String, OwnedValue> = reply.body().deserialize()?;
            let kind = props.get("Type").and_then(extract_string).unwrap_or_default();
            let data = props.get("ManufacturerData").and_then(extract_mfg_ffff);
            self.calls.lock().unwrap().push(format!("register {} {kind} {data:?}", path.as_str()));
            Ok(())
        }

        fn unregister_advertisement(&self, path: OwnedObjectPath) {
            self.calls.lock().unwrap().push(format!("unregister {}", path.as_str()));
        }
    }

    #[test]
    fn beacon_registers_with_bluez_and_follows_its_adapter() {
        let Some(bus) = TestBus::start() else {
            eprintln!("skipped: dbus-daemon isn't installed");
            return;
        };
        let manager = FakeManager::default();
        let bluez = bus
            .connect()
            .name(BLUEZ)
            .unwrap()
            .serve_at("/org/bluez/hci0", manager.clone())
            .unwrap()
            .build()
            .unwrap();
        let id = DeviceId([0x5A; 32]);
        let (conn, mut beacon) = Beacon::serve(bus.connect(), &id).unwrap();
        let register = format!(
            "register {ADVERTISEMENT_PATH} broadcast {:?}",
            Some(build_manufacturer_payload(&id).to_vec())
        );
        let unregister = format!("unregister {ADVERTISEMENT_PATH}");
        let (on, off) = (adapter("/org/bluez/hci0", true, true), adapter("/org/bluez/hci0", false, true));

        beacon.sync(&conn, &on);
        beacon.sync(&conn, &on);
        assert_eq!(manager.take(), std::slice::from_ref(&register));

        // BlueZ dropped it, so it goes back.
        bluez
            .call_method(
                conn.unique_name().map(|n| n.as_str()),
                ADVERTISEMENT_PATH,
                Some("org.bluez.LEAdvertisement1"),
                "Release",
                &(),
            )
            .unwrap();
        beacon.sync(&conn, &on);
        assert_eq!(manager.take(), std::slice::from_ref(&register));

        // Bluetooth turned off, then on again.
        beacon.sync(&conn, &off);
        beacon.sync(&conn, &off);
        assert_eq!(manager.take(), std::slice::from_ref(&unregister));
        beacon.sync(&conn, &on);
        assert_eq!(manager.take(), [register]);

        beacon.stop(&conn);
        assert_eq!(manager.take(), [unregister]);
    }

    #[test]
    fn start_and_stop_never_panic_when_ble_radio_is_absent_or_present() {
        let id = DeviceId([0x42; 32]);
        start(id);
        stop();
    }

    #[test]
    fn start_and_stop_are_idempotent_without_bluez() {
        let id = DeviceId([0x24; 32]);
        start(id);
        start(id);
        stop();
        stop();
    }
}
