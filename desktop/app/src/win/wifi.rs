// SPDX-License-Identifier: GPL-3.0-or-later
//! Wi-Fi through the Native Wifi API (`wlanapi.dll`): the networks Windows has
//! saved, whether one is in range, and joining one with its saved profile.
//! NectarLink never sees or stores a Wi-Fi password: joining uses the profile
//! Windows keeps from when the user joined that network themselves.

use windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, HANDLE},
        NetworkManagement::WiFi::{
            WLAN_API_VERSION_2_0, WLAN_AVAILABLE_NETWORK, WLAN_AVAILABLE_NETWORK_CONNECTED,
            WLAN_AVAILABLE_NETWORK_INCLUDE_ALL_MANUAL_HIDDEN_PROFILES, WLAN_AVAILABLE_NETWORK_LIST,
            WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST, WLAN_PROFILE_INFO,
            WLAN_PROFILE_INFO_LIST, WlanCloseHandle, WlanConnect, WlanEnumInterfaces, WlanFreeMemory,
            WlanGetAvailableNetworkList, WlanGetProfileList, WlanOpenHandle, WlanScan,
            dot11_BSS_type_infrastructure, wlan_connection_mode_profile,
        },
    },
    core::{GUID, PCWSTR},
};

/// A network in range, as Windows lists it for a Wi-Fi adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InRange {
    /// The saved profile it matches ("" when Windows has none for it).
    pub profile: String,
    pub connectable: bool,
    pub connected: bool,
}

/// An open WLAN client handle, closed when dropped.
struct Client(HANDLE);

impl Client {
    fn open() -> Option<Client> {
        let mut negotiated = 0u32;
        let mut handle = HANDLE::default();
        // SAFETY: out pointers to locals; the handle is closed in Drop.
        let rc = unsafe { WlanOpenHandle(WLAN_API_VERSION_2_0, None, &mut negotiated, &mut handle) };
        (rc == ERROR_SUCCESS.0).then_some(Client(handle))
    }

    /// The Wi-Fi adapters' GUIDs.
    fn interfaces(&self) -> Vec<GUID> {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: a valid client handle and an out pointer; the list is freed below.
        if unsafe { WlanEnumInterfaces(self.0, None, &mut list) } != ERROR_SUCCESS.0 || list.is_null() {
            return Vec::new();
        }
        // SAFETY: `list` points to a WLAN_INTERFACE_INFO_LIST with `dwNumberOfItems`
        // entries laid out from `InterfaceInfo`, valid until WlanFreeMemory.
        let guids = unsafe {
            let count = (*list).dwNumberOfItems as usize;
            let first = std::ptr::addr_of!((*list).InterfaceInfo).cast::<WLAN_INTERFACE_INFO>();
            (0..count).map(|i| (*first.add(i)).InterfaceGuid).collect()
        };
        // SAFETY: memory WlanEnumInterfaces allocated.
        unsafe { WlanFreeMemory(list.cast()) };
        guids
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: the handle WlanOpenHandle returned, closed once.
        unsafe { WlanCloseHandle(self.0, None) };
    }
}

fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

/// The Wi-Fi networks Windows has saved, by profile name, sorted, without repeats.
pub fn saved_networks() -> Vec<String> {
    let Some(client) = Client::open() else { return Vec::new() };
    let mut names = Vec::new();
    for guid in client.interfaces() {
        let mut list: *mut WLAN_PROFILE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: a valid handle and adapter GUID; the list is freed below.
        if unsafe { WlanGetProfileList(client.0, &guid, None, &mut list) } != ERROR_SUCCESS.0
            || list.is_null()
        {
            continue;
        }
        // SAFETY: as in Client::interfaces, for WLAN_PROFILE_INFO_LIST.
        unsafe {
            let count = (*list).dwNumberOfItems as usize;
            let first = std::ptr::addr_of!((*list).ProfileInfo).cast::<WLAN_PROFILE_INFO>();
            names.extend((0..count).map(|i| wide_to_string(&(*first.add(i)).strProfileName)));
            WlanFreeMemory(list.cast());
        }
    }
    tidy(names)
}

/// Sorted case-insensitively, empty names and repeats left out.
fn tidy(mut names: Vec<String>) -> Vec<String> {
    names.retain(|n| !n.trim().is_empty());
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    names.dedup();
    names
}

/// Asks every Wi-Fi adapter to look for networks again (results come within a few seconds).
pub fn scan() {
    let Some(client) = Client::open() else { return };
    for guid in client.interfaces() {
        // SAFETY: a valid handle and adapter GUID; no SSID or IE data.
        unsafe { WlanScan(client.0, &guid, None, None, None) };
    }
}

/// The networks in range on the first adapter that lists any.
fn in_range(client: &Client) -> Option<(GUID, Vec<InRange>)> {
    for guid in client.interfaces() {
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        let flags = WLAN_AVAILABLE_NETWORK_INCLUDE_ALL_MANUAL_HIDDEN_PROFILES;
        // SAFETY: a valid handle and adapter GUID; the list is freed below.
        let rc = unsafe { WlanGetAvailableNetworkList(client.0, &guid, flags, None, &mut list) };
        if rc != ERROR_SUCCESS.0 || list.is_null() {
            continue;
        }
        // SAFETY: as in Client::interfaces, for WLAN_AVAILABLE_NETWORK_LIST.
        let networks: Vec<InRange> = unsafe {
            let count = (*list).dwNumberOfItems as usize;
            let first = std::ptr::addr_of!((*list).Network).cast::<WLAN_AVAILABLE_NETWORK>();
            let items = (0..count)
                .map(|i| {
                    let n = &*first.add(i);
                    InRange {
                        profile: wide_to_string(&n.strProfileName),
                        connectable: n.bNetworkConnectable.as_bool(),
                        connected: n.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    }
                })
                .collect();
            WlanFreeMemory(list.cast());
            items
        };
        return Some((guid, networks));
    }
    None
}

/// Where a saved network stands right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    NotInRange,
    InRange,
    Connected,
}

pub fn reach(profile: &str) -> Reach {
    let Some(client) = Client::open() else { return Reach::NotInRange };
    let Some((_, networks)) = in_range(&client) else { return Reach::NotInRange };
    reach_in(&networks, profile)
}

fn reach_in(networks: &[InRange], profile: &str) -> Reach {
    let mine = networks.iter().filter(|n| n.profile == profile);
    if mine.clone().any(|n| n.connected) {
        Reach::Connected
    } else if mine.clone().any(|n| n.connectable) {
        Reach::InRange
    } else {
        Reach::NotInRange
    }
}

/// Joins the saved network `profile` (Windows uses its stored settings and password).
/// Returns once Windows has started connecting; check [`reach`] for the result.
pub fn connect(profile: &str) -> Result<(), String> {
    let client = Client::open().ok_or("Wi-Fi isn't available")?;
    let (guid, _) = in_range(&client).ok_or("no Wi-Fi adapter")?;
    let wide: Vec<u16> = profile.encode_utf16().chain(std::iter::once(0)).collect();
    let params = WLAN_CONNECTION_PARAMETERS {
        wlanConnectionMode: wlan_connection_mode_profile,
        strProfile: PCWSTR(wide.as_ptr()),
        pDot11Ssid: std::ptr::null_mut(),
        pDesiredBssidList: std::ptr::null_mut(),
        dot11BssType: dot11_BSS_type_infrastructure,
        dwFlags: 0,
    };
    // SAFETY: valid handle and GUID; `params` and the profile name outlive the call.
    let rc = unsafe { WlanConnect(client.0, &guid, &params, None) };
    if rc == ERROR_SUCCESS.0 { Ok(()) } else { Err(format!("WlanConnect failed ({rc})")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(profile: &str, connectable: bool, connected: bool) -> InRange {
        InRange { profile: profile.into(), connectable, connected }
    }

    #[test]
    fn saved_network_names_are_tidied() {
        let names =
            vec!["home".into(), "Pixel 9".into(), "".into(), "Home".into(), "home".into(), "  ".into()];
        assert_eq!(tidy(names), ["Home", "home", "Pixel 9"]);
    }

    #[test]
    fn a_saved_network_is_connected_in_range_or_not() {
        // Windows lists a network once per profile and once without: only the profile counts.
        let list = [net("Home", true, true), net("", true, false), net("Pixel 9", true, false)];
        assert_eq!(reach_in(&list, "Home"), Reach::Connected);
        assert_eq!(reach_in(&list, "Pixel 9"), Reach::InRange);
        assert_eq!(reach_in(&list, "Office"), Reach::NotInRange);
        assert_eq!(
            reach_in(&[net("Pixel 9", false, false)], "Pixel 9"),
            Reach::NotInRange,
            "seen, can't connect"
        );
    }

    /// Reads the real saved networks: nothing changes on this PC.
    #[test]
    fn lists_saved_networks_without_failing() {
        let names = saved_networks();
        assert!(names.iter().all(|n| !n.trim().is_empty()));
    }
}
