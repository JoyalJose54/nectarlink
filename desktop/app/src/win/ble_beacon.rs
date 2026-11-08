// SPDX-License-Identifier: GPL-3.0-or-later
//! PC BLE proximity beacon (`docs/protocol/discovery.md`): broadcasts a compact
//! 13-byte Bluetooth Low Energy manufacturer-data frame (`COMPANY_ID = 0xFFFF`,
//! `"NCTR\x01"` + 8-byte `DeviceId` prefix) so nearby phones can wake discovery
//! and reconnect immediately when in range.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use nectarlink_core::DeviceId;
use windows::{
    Devices::Bluetooth::Advertisement::{
        BluetoothLEAdvertisementPublisher, BluetoothLEAdvertisementPublisherStatus,
        BluetoothLEAdvertisementPublisherStatusChangedEventArgs, BluetoothLEAdvertisementReceivedEventArgs,
        BluetoothLEAdvertisementWatcher, BluetoothLEManufacturerData,
    },
    Foundation::TypedEventHandler,
    Storage::Streams::{DataReader, DataWriter},
};

/// Bluetooth SIG test/internal company identifier used for NectarLink proximity frames.
pub const COMPANY_ID: u16 = 0xFFFF;

/// 5-byte magic header (`"NCTR"` + protocol version `0x01`).
pub const MAGIC: &[u8; 5] = b"NCTR\x01";

/// Near-range RSSI threshold (dBm) that arms walk-away detection.
pub const NEAR_RSSI_DBM: i16 = -72;

/// Walk-away RSSI threshold (dBm) below which `phone_walked_away` fires.
pub const WALK_AWAY_RSSI_DBM: i16 = -85;

static PUBLISHER: OnceLock<Mutex<Option<BluetoothLEAdvertisementPublisher>>> = OnceLock::new();
static WATCHER: OnceLock<Mutex<Option<BluetoothLEAdvertisementWatcher>>> = OnceLock::new();
static RSSI_TRACKER: OnceLock<Mutex<RssiTracker>> = OnceLock::new();

fn publisher_slot() -> &'static Mutex<Option<BluetoothLEAdvertisementPublisher>> {
    PUBLISHER.get_or_init(|| Mutex::new(None))
}

fn watcher_slot() -> &'static Mutex<Option<BluetoothLEAdvertisementWatcher>> {
    WATCHER.get_or_init(|| Mutex::new(None))
}

fn rssi_tracker() -> &'static Mutex<RssiTracker> {
    RSSI_TRACKER.get_or_init(|| Mutex::new(RssiTracker::default()))
}

/// Rolling BLE RSSI hysteresis tracker per 8-byte peer ID prefix (`docs/PLAN.md` §8.6 / 2.1).
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
    /// Records a new RSSI reading (in dBm) for `prefix` and returns `true` when
    /// the peer transitions from armed-near (`>= -72 dBm`) to walked-away (`<= -85 dBm`).
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

/// Builds the 13-byte manufacturer-specific payload for `device_id`:
/// - Bytes `0..5`: [`MAGIC`] (`b"NCTR\x01"`)
/// - Bytes `5..13`: first 8 bytes of `device_id.as_bytes()` (Ed25519 public key prefix)
pub fn build_manufacturer_payload(device_id: &DeviceId) -> [u8; 13] {
    let mut out = [0u8; 13];
    out[0..5].copy_from_slice(MAGIC);
    out[5..13].copy_from_slice(&device_id.as_bytes()[0..8]);
    out
}

/// Starts advertising the NectarLink BLE proximity beacon for `device_id`.
///
/// The RSSI watcher (`try_start_watcher`) stays off: phones don't broadcast a
/// beacon yet, so it would scan all the time and only ever hear other PCs.
pub fn start(device_id: DeviceId) {
    // Phones find this PC by it ("Wake when PC is nearby"), so say when it can't run.
    if let Err(e) = try_start(&device_id) {
        tracing::warn!(error = %e, "this PC can't broadcast its Bluetooth beacon");
    }
}

/// Stops the active BLE proximity beacon publisher and RSSI watcher, if running.
pub fn stop() {
    if let Ok(mut guard) = publisher_slot().lock()
        && let Some(publisher) = guard.take()
    {
        let _ = publisher.Stop();
    }
    if let Ok(mut guard) = watcher_slot().lock()
        && let Some(watcher) = guard.take()
    {
        let _ = watcher.Stop();
    }
}

fn try_start(device_id: &DeviceId) -> windows::core::Result<()> {
    let payload = build_manufacturer_payload(device_id);
    let writer = DataWriter::new()?;
    writer.WriteBytes(&payload)?;
    let buffer = writer.DetachBuffer()?;

    let mfg_data = BluetoothLEManufacturerData::Create(COMPANY_ID, &buffer)?;
    let publisher = BluetoothLEAdvertisementPublisher::new()?;
    publisher.Advertisement()?.ManufacturerData()?.Append(&mfg_data)?;
    // Windows can accept Start and stop a moment later (an adapter that can't
    // advertise, Bluetooth turned off), so log why it stops.
    publisher.StatusChanged(&TypedEventHandler::<
        BluetoothLEAdvertisementPublisher,
        BluetoothLEAdvertisementPublisherStatusChangedEventArgs,
    >::new(|_, args| {
        if let Some(args) = args.as_ref()
            && args.Status()? == BluetoothLEAdvertisementPublisherStatus::Aborted
        {
            tracing::warn!(error = ?args.Error()?, "the Bluetooth beacon stopped");
        }
        Ok(())
    }))?;
    publisher.Start()?;

    if let Ok(mut guard) = publisher_slot().lock()
        && let Some(prev) = guard.replace(publisher)
    {
        let _ = prev.Stop();
    }
    tracing::info!("broadcasting the Bluetooth beacon");
    Ok(())
}

fn try_start_watcher(own_id: DeviceId) -> windows::core::Result<()> {
    let own_prefix: [u8; 8] = own_id.as_bytes()[0..8].try_into().unwrap_or([0u8; 8]);
    let watcher = BluetoothLEAdvertisementWatcher::new()?;
    watcher.Received(&TypedEventHandler::<
        BluetoothLEAdvertisementWatcher,
        BluetoothLEAdvertisementReceivedEventArgs,
    >::new(move |_, args| {
        if let Some(args) = args.as_ref() {
            let rssi = args.RawSignalStrengthInDBm().unwrap_or(-127);
            if let Ok(adv) = args.Advertisement()
                && let Ok(list) = adv.GetManufacturerDataByCompanyId(COMPANY_ID)
                && let Ok(count) = list.Size()
            {
                for i in 0..count {
                    if let Ok(item) = list.GetAt(i)
                        && let Ok(buf) = item.Data()
                        && let Ok(len) = buf.Length()
                        && len == 13
                        && let Ok(reader) = DataReader::FromBuffer(&buf)
                    {
                        let mut bytes = [0u8; 13];
                        if reader.ReadBytes(&mut bytes).is_ok() && &bytes[0..5] == MAGIC {
                            let mut prefix = [0u8; 8];
                            prefix.copy_from_slice(&bytes[5..13]);
                            if prefix != own_prefix
                                && let Ok(mut tracker) = rssi_tracker().lock()
                                && tracker.record(prefix, rssi)
                            {
                                crate::automations::fire_trigger(
                                    crate::automations::TRIGGER_PHONE_WALKED_AWAY,
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }))?;
    watcher.Start()?;
    if let Ok(mut guard) = watcher_slot().lock()
        && let Some(prev) = guard.replace(watcher)
    {
        let _ = prev.Stop();
    }
    Ok(())
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
    fn rssi_tracker_fires_walk_away_only_after_armed_near() {
        let mut tracker = RssiTracker::default();
        let peer = [1, 2, 3, 4, 5, 6, 7, 8];
        // Far readings before ever being near should not trigger walk-away.
        for _ in 0..4 {
            assert!(!tracker.record(peer, -90));
        }
        // Coming into near range arms the tracker.
        for _ in 0..4 {
            assert!(!tracker.record(peer, -65));
        }
        // Walking away below -85 dBm triggers once, then disarms until near again.
        assert!(!tracker.record(peer, -88));
        assert!(!tracker.record(peer, -90));
        assert!(!tracker.record(peer, -92));
        assert!(tracker.record(peer, -90));
        assert!(!tracker.record(peer, -92));
    }

    #[test]
    fn start_and_stop_never_panic_when_ble_radio_is_absent_or_present() {
        let id = DeviceId([0x42; 32]);
        start(id);
        stop();
    }
}
