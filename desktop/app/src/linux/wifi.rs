// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux Wi-Fi scan and connection helper for Instant Hotspot via NetworkManager (`nmcli`).

use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InRange {
    pub profile: String,
    pub connectable: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    NotInRange,
    InRange,
    Connected,
}

pub fn unescape_nmcli(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(':') => out.push(':'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn split_nmcli_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(':') => current.push(':'),
                Some('\\') => current.push('\\'),
                Some(other) => current.push(other),
                None => current.push('\\'),
            }
        } else if ch == ':' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    fields.push(current);
    fields
}

fn tidy(mut names: Vec<String>) -> Vec<String> {
    names.retain(|n| !n.trim().is_empty());
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    names.dedup();
    names
}

fn parse_saved_profiles(stdout: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in stdout.lines() {
        // Format from `nmcli -t -f TYPE,NAME connection show`:
        // 802-11-wireless:MyHotspot
        if let Some((kind, name)) = line.split_once(':')
            && (kind == "802-11-wireless" || kind == "wifi")
        {
            let unescaped = unescape_nmcli(name);
            let trimmed = unescaped.trim();
            if !trimmed.is_empty() {
                names.push(trimmed.to_string());
            }
        }
    }
    tidy(names)
}

pub fn saved_networks() -> Vec<String> {
    let Ok(out) = Command::new("nmcli").args(["-t", "-f", "TYPE,NAME", "connection", "show"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_saved_profiles(&String::from_utf8_lossy(&out.stdout))
}

/// Parses `nmcli -t -f ACTIVE,SSID dev wifi` output, finding the active `yes:<ssid>` line
/// and unescaping `\:` -> `:` and `\\` -> `\`.
pub fn parse_active_dev_wifi(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        if let Some(raw_ssid) = line.trim().strip_prefix("yes:") {
            let ssid = unescape_nmcli(raw_ssid);
            let trimmed = ssid.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Parses `nmcli -t -f NAME,TYPE connection show --active` output as a fallback.
pub fn parse_active_connection_show(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let fields = split_nmcli_fields(line.trim());
        if fields.len() >= 2 {
            let kind = fields[fields.len() - 1].trim();
            if kind == "802-11-wireless" || kind == "wifi" {
                let name = fields[..fields.len() - 1].join(":");
                let trimmed = name.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

/// Returns the currently active 802.11 Wi-Fi SSID (or active Wi-Fi connection name).
pub fn current_profile() -> Option<String> {
    if let Ok(out) = Command::new("nmcli").args(["-t", "-f", "ACTIVE,SSID", "dev", "wifi"]).output()
        && out.status.success()
        && let Some(ssid) = parse_active_dev_wifi(&String::from_utf8_lossy(&out.stdout))
    {
        return Some(ssid);
    }

    let out = Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE", "connection", "show", "--active"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_active_connection_show(&String::from_utf8_lossy(&out.stdout))
}

pub fn scan() {
    let _ = Command::new("nmcli").args(["device", "wifi", "rescan"]).output();
}

fn parse_in_range(stdout: &str) -> Vec<InRange> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        // Format from `nmcli -t -f ACTIVE,SSID device wifi list`:
        // yes:MyHotspot
        if let Some((active, raw_ssid)) = line.split_once(':') {
            let unescaped = unescape_nmcli(raw_ssid);
            let ssid = unescaped.trim();
            if !ssid.is_empty() {
                out.push(InRange {
                    profile: ssid.to_string(),
                    connectable: true,
                    connected: active.trim() == "yes",
                });
            }
        }
    }
    out
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

pub fn reach(profile: &str) -> Reach {
    let Ok(out) = Command::new("nmcli").args(["-t", "-f", "ACTIVE,SSID", "device", "wifi", "list"]).output()
    else {
        return Reach::NotInRange;
    };
    if !out.status.success() {
        return Reach::NotInRange;
    }
    let networks = parse_in_range(&String::from_utf8_lossy(&out.stdout));
    reach_in(&networks, profile)
}

pub fn connect(profile: &str) -> Result<(), String> {
    let trimmed = profile.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') || trimmed.chars().any(char::is_control) {
        return Err("invalid Wi-Fi profile name".into());
    }
    let out = Command::new("nmcli")
        .args(["connection", "up", "id", trimmed])
        .output()
        .map_err(|e| format!("failed to run nmcli: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Err(if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("nmcli exited with {}", out.status)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_tidies_saved_wifi_profiles() {
        let sample =
            "802-11-wireless:Pixel 9a\n802-3-ethernet:Wired connection 1\nwifi:home\nwifi:Home\nwifi:home\n";
        let names = parse_saved_profiles(sample);
        assert_eq!(names, vec!["Home", "home", "Pixel 9a"]);
    }

    #[test]
    fn reach_in_reports_connected_in_range_or_not() {
        let sample = "yes:Home\nno:Pixel 9a\n";
        let list = parse_in_range(sample);
        assert_eq!(reach_in(&list, "Home"), Reach::Connected);
        assert_eq!(reach_in(&list, "Pixel 9a"), Reach::InRange);
        assert_eq!(reach_in(&list, "Office"), Reach::NotInRange);
    }

    #[test]
    fn parses_active_dev_wifi_with_escaped_colons_and_backslashes() {
        let output = "no:NeighborWiFi\nyes:Cafe\\:Guest\\\\5GHz\nno:OtherNet\n";
        assert_eq!(parse_active_dev_wifi(output), Some("Cafe:Guest\\5GHz".to_string()));
        assert_eq!(parse_active_dev_wifi("no:NeighborWiFi\nyes:\n"), None);

        let fallback_output = "Wired connection 1:802-3-ethernet\nMy\\:Hotspot\\\\Phone:802-11-wireless\n";
        assert_eq!(parse_active_connection_show(fallback_output), Some("My:Hotspot\\Phone".to_string()));
    }
}
