// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux speaker volume, mute, output device enumeration, microphone mute
//! state, and event-driven change notifications via PipeWire (`wpctl`) and
//! PulseAudio (`pactl`).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::Duration;

use nectarlink_core::AudioOutputDevice;

static WATCH_STARTED: OnceLock<()> = OnceLock::new();

/// Parses `wpctl get-volume` output such as `"Volume: 0.45"` or
/// `"Volume: 0.45 [MUTED]"` into `(volume_percent, muted)`.
pub(crate) fn parse_wpctl_volume(output: &str) -> Option<(u8, bool)> {
    let muted = output.contains("[MUTED]");
    let after_colon = output.split_once("Volume:")?.1;
    let vol_token = after_colon.split_whitespace().next()?;
    let val = vol_token.parse::<f32>().ok()?;
    if !val.is_finite() {
        return None;
    }
    let pct = (val * 100.0).round().clamp(0.0, 100.0) as u8;
    Some((pct, muted))
}

/// Parses `pactl get-sink-mute` / `pactl get-source-mute` output such as
/// `"Mute: yes"` or `"Mute: no"`.
pub(crate) fn parse_pactl_mute(mute_out: &str) -> Option<bool> {
    let rhs = mute_out.split_once(':').map(|(_, r)| r).unwrap_or(mute_out).trim();
    if rhs.eq_ignore_ascii_case("yes") || rhs.eq_ignore_ascii_case("true") || rhs == "1" {
        Some(true)
    } else if rhs.eq_ignore_ascii_case("no") || rhs.eq_ignore_ascii_case("false") || rhs == "0" {
        Some(false)
    } else {
        None
    }
}

/// Parses `pactl get-sink-volume` and optional `pactl get-sink-mute` output
/// into `(volume_percent, muted)`.
pub(crate) fn parse_pactl_volume_and_mute(vol_out: &str, mute_out: Option<&str>) -> Option<(u8, bool)> {
    let pct_token = vol_out.split_whitespace().find_map(|tok| tok.strip_suffix('%'))?;
    let pct_val = pct_token.trim().parse::<f32>().ok()?;
    if !pct_val.is_finite() {
        return None;
    }
    let pct = pct_val.round().clamp(0.0, 100.0) as u8;
    let muted = mute_out.and_then(parse_pactl_mute).unwrap_or(false);
    Some((pct, muted))
}

/// Parses `pactl list sinks` output along with `pactl get-default-sink` into
/// sorted `AudioOutputDevice` entries (default first, then alphabetical by name).
pub(crate) fn parse_pactl_sinks(list_out: &str, default_sink: Option<&str>) -> Vec<AudioOutputDevice> {
    let default_trimmed = default_sink.map(str::trim).filter(|s| !s.is_empty());
    let mut out = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_desc: Option<String> = None;

    let flush = |id: &mut Option<String>, desc: &mut Option<String>, out: &mut Vec<AudioOutputDevice>| {
        if let Some(sink_id) = id.take() {
            let name = desc.take().filter(|d| !d.is_empty()).unwrap_or_else(|| sink_id.clone());
            let is_default = default_trimmed == Some(sink_id.as_str());
            out.push(AudioOutputDevice { id: sink_id, name, is_default });
        } else {
            *desc = None;
        }
    };

    for raw_line in list_out.lines() {
        let line = raw_line.trim();
        if line.starts_with("Sink #") {
            flush(&mut current_id, &mut current_desc, &mut out);
        } else if let Some(rest) = line.strip_prefix("Name:") {
            let id = rest.trim();
            if !id.is_empty() && current_id.is_none() {
                current_id = Some(id.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("Description:") {
            let desc = rest.trim();
            if !desc.is_empty() && current_desc.is_none() {
                current_desc = Some(desc.to_owned());
            }
        }
    }
    flush(&mut current_id, &mut current_desc, &mut out);

    out.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Parses the `Sinks:` section of `wpctl status` into sorted `AudioOutputDevice`
/// entries (default first, then alphabetical by name).
pub(crate) fn parse_wpctl_status_sinks(status_out: &str) -> Vec<AudioOutputDevice> {
    let mut out = Vec::new();
    let mut in_sinks = false;

    for raw_line in status_out.lines() {
        let stripped = raw_line
            .trim_start_matches(|c: char| {
                c.is_whitespace() || matches!(c, '│' | '├' | '└' | '─' | '|' | '+' | '-')
            })
            .trim();
        if stripped.is_empty() {
            continue;
        }
        if stripped == "Sinks:" {
            in_sinks = true;
            continue;
        }
        if !in_sinks {
            continue;
        }
        let has_tree_indent =
            raw_line.starts_with(char::is_whitespace) || raw_line.starts_with(['│', '├', '└', '|']);
        if !has_tree_indent || stripped.ends_with(':') {
            break;
        }

        let (is_default, after_star) = match stripped.strip_prefix('*') {
            Some(rest) => (true, rest.trim_start()),
            None => (false, stripped),
        };
        let Some((id_part, rest)) = after_star.split_once('.') else {
            continue;
        };
        let id = id_part.trim();
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let raw_name = match rest.rfind(" [") {
            Some(idx) => &rest[..idx],
            None => rest,
        };
        let name = raw_name.trim();
        if !name.is_empty() {
            out.push(AudioOutputDevice { id: id.to_owned(), name: name.to_owned(), is_default });
        }
    }

    out.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Resolves a numeric PipeWire sink node ID for `wpctl set-default` from `target`
/// (which may already be numeric, a display name in `wpctl status`, or a node name
/// in `wpctl status --name`).
pub(crate) fn resolve_wpctl_sink_node_id(
    status_out: &str,
    status_name_out: Option<&str>,
    target: &str,
) -> Option<String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Some(trimmed.to_owned());
    }
    for sink in parse_wpctl_status_sinks(status_out) {
        if sink.id == trimmed || sink.name.eq_ignore_ascii_case(trimmed) {
            return Some(sink.id);
        }
    }
    if let Some(name_out) = status_name_out {
        for sink in parse_wpctl_status_sinks(name_out) {
            if sink.id == trimmed || sink.name.eq_ignore_ascii_case(trimmed) {
                return Some(sink.id);
            }
        }
    }
    None
}

/// Returns `true` when a `pactl subscribe` event line indicates a sink, source,
/// or server state/device change (ignoring per-stream `sink-input` / `source-output` noise).
pub(crate) fn parse_pactl_subscribe_line(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with("Event '") {
        return false;
    }
    (trimmed.contains(" on sink #")
        || trimmed.ends_with(" on sink")
        || trimmed.contains(" on source #")
        || trimmed.ends_with(" on source")
        || trimmed.contains(" on server #")
        || trimmed.ends_with(" on server"))
        && !trimmed.contains("sink-input")
        && !trimmed.contains("source-output")
}

const CMD_TIMEOUT: Duration = Duration::from_millis(800);

/// Reads the default speaker endpoint's `(volume_percent, muted)`.
pub fn speaker_state() -> Option<(u8, bool)> {
    if let Some(out) = super::run_cmd_timeout("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"], CMD_TIMEOUT)
        && out.status.success()
        && let Some(state) = parse_wpctl_volume(&String::from_utf8_lossy(&out.stdout))
    {
        return Some(state);
    }

    if let Some(vol_out) =
        super::run_cmd_timeout("pactl", &["get-sink-volume", "@DEFAULT_SINK@"], CMD_TIMEOUT)
        && vol_out.status.success()
    {
        let mute_out = super::run_cmd_timeout("pactl", &["get-sink-mute", "@DEFAULT_SINK@"], CMD_TIMEOUT)
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        if let Some(state) =
            parse_pactl_volume_and_mute(&String::from_utf8_lossy(&vol_out.stdout), mute_out.as_deref())
        {
            return Some(state);
        }
    }

    None
}

/// Sets the default speaker endpoint's master volume (`0..=100`).
pub fn set_speaker_volume(level: u8) -> Result<(), String> {
    let clamped = level.min(100);
    let arg = format!("{clamped}%");
    if super::run_cmd_timeout("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &arg], CMD_TIMEOUT)
        .is_some_and(|o| o.status.success())
    {
        return Ok(());
    }
    let _ = super::run_cmd_timeout("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &arg], CMD_TIMEOUT);
    Ok(())
}

/// Sets the default speaker endpoint's mute state.
pub fn set_speaker_mute(muted: bool) -> Result<(), String> {
    let flag = if muted { "1" } else { "0" };
    if super::run_cmd_timeout("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", flag], CMD_TIMEOUT)
        .is_some_and(|o| o.status.success())
    {
        return Ok(());
    }
    let _ = super::run_cmd_timeout("pactl", &["set-sink-mute", "@DEFAULT_SINK@", flag], CMD_TIMEOUT);
    Ok(())
}

/// Steps the default speaker volume up by one increment (2%) and unmutes if muted.
pub fn speaker_volume_up() -> Result<(u8, bool), String> {
    let (current, muted) = speaker_state().unwrap_or((50, false));
    let next = current.saturating_add(2).min(100);
    set_speaker_volume(next)?;
    if muted {
        set_speaker_mute(false)?;
    }
    Ok((next, false))
}

/// Steps the default speaker volume down by one increment (2%).
pub fn speaker_volume_down() -> Result<(u8, bool), String> {
    let (current, muted) = speaker_state().unwrap_or((50, false));
    let next = current.saturating_sub(2);
    set_speaker_volume(next)?;
    Ok((next, muted))
}

/// Toggles mute on the default speaker endpoint and returns the new muted state.
pub fn toggle_speaker_mute() -> Result<bool, String> {
    let (_, muted) = speaker_state().unwrap_or((50, false));
    let next = !muted;
    set_speaker_mute(next)?;
    Ok(next)
}

/// Reads whether the default capture (microphone) endpoint is muted.
pub fn mic_muted() -> Option<bool> {
    if let Some(out) = super::run_cmd_timeout("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"], CMD_TIMEOUT)
        && out.status.success()
        && let Some((_, muted)) = parse_wpctl_volume(&String::from_utf8_lossy(&out.stdout))
    {
        return Some(muted);
    }

    if let Some(out) = super::run_cmd_timeout("pactl", &["get-source-mute", "@DEFAULT_SOURCE@"], CMD_TIMEOUT)
        && out.status.success()
        && let Some(muted) = parse_pactl_mute(&String::from_utf8_lossy(&out.stdout))
    {
        return Some(muted);
    }

    None
}

/// Toggles mute on the default capture (microphone) endpoint and returns the new muted state.
pub fn toggle_mic_mute() -> Result<bool, String> {
    let current = mic_muted().unwrap_or(false);
    let next = !current;
    let flag = if next { "1" } else { "0" };
    if super::run_cmd_timeout("wpctl", &["set-mute", "@DEFAULT_AUDIO_SOURCE@", flag], CMD_TIMEOUT)
        .is_some_and(|o| o.status.success())
    {
        return Ok(next);
    }
    let _ = super::run_cmd_timeout("pactl", &["set-source-mute", "@DEFAULT_SOURCE@", flag], CMD_TIMEOUT);
    Ok(next)
}

/// Lists active audio output sinks on this PC, marking the current default
/// output (`is_default: true`) and sorting the default first.
pub fn output_devices() -> Vec<AudioOutputDevice> {
    if let Some(wpctl_out) = super::run_cmd_timeout("wpctl", &["status"], CMD_TIMEOUT)
        && wpctl_out.status.success()
    {
        let parsed = parse_wpctl_status_sinks(&String::from_utf8_lossy(&wpctl_out.stdout));
        if !parsed.is_empty() {
            return parsed;
        }
    }

    let default_sink = super::run_cmd_timeout("pactl", &["get-default-sink"], CMD_TIMEOUT)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty());

    if let Some(list_out) = super::run_cmd_timeout("pactl", &["list", "sinks"], CMD_TIMEOUT)
        && list_out.status.success()
    {
        let parsed = parse_pactl_sinks(&String::from_utf8_lossy(&list_out.stdout), default_sink.as_deref());
        if !parsed.is_empty() {
            return parsed;
        }
    }

    Vec::new()
}

/// Switches the system default output sink to `id`.
pub fn set_default_output_device(id: &str) -> Result<(), String> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') || trimmed.chars().any(char::is_control) {
        return Err("output device id is invalid".to_string());
    }

    let status_out = super::run_cmd_timeout("wpctl", &["status"], CMD_TIMEOUT)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let status_name_out = super::run_cmd_timeout("wpctl", &["status", "--name"], CMD_TIMEOUT)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());

    if let Some(node_id) = resolve_wpctl_sink_node_id(&status_out, status_name_out.as_deref(), trimmed)
        && super::run_cmd_timeout("wpctl", &["set-default", &node_id], CMD_TIMEOUT)
            .is_some_and(|o| o.status.success())
    {
        return Ok(());
    }

    if super::run_cmd_timeout("pactl", &["set-default-sink", trimmed], CMD_TIMEOUT)
        .is_some_and(|o| o.status.success())
    {
        return Ok(());
    }

    Err(format!("audio backend could not set default output device '{trimmed}'"))
}

/// Subscribes to PipeWire/PulseAudio sink/source/server events and invokes
/// `changed` after coalescing rapid event bursts over 150 ms.
pub fn watch(changed: fn()) {
    if WATCH_STARTED.set(()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("audio-watch".into()).spawn(move || watch_thread(changed));
}

fn watch_thread(changed: fn()) {
    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new("pactl");
    cmd.arg("subscribe").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    // SAFETY: `prctl(PR_SET_PDEATHSIG, SIGTERM)` is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else {
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };

    let (tx, rx) = mpsc::channel::<()>();
    let _ = std::thread::Builder::new().name("audio-watch-reader".into()).spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if parse_pactl_subscribe_line(&line) && tx.send(()).is_err() {
                break;
            }
        }
    });

    while rx.recv().is_ok() {
        while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}
        changed();
    }

    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wpctl_volume_handles_normal_and_muted_lines() {
        assert_eq!(parse_wpctl_volume("Volume: 0.45\n"), Some((45, false)));
        assert_eq!(parse_wpctl_volume("Volume: 0.72 [MUTED]\n"), Some((72, true)));
        assert_eq!(parse_wpctl_volume("Volume: 1.35"), Some((100, false)));
        assert_eq!(parse_wpctl_volume("garbage"), None);
    }

    #[test]
    fn parse_pactl_volume_and_mute_extracts_percentage_and_mute() {
        let vol = "Volume: front-left: 32768 /  50% / -18.06 dB,   front-right: 32768 /  50% / -18.06 dB\n";
        assert_eq!(parse_pactl_volume_and_mute(vol, Some("Mute: no\n")), Some((50, false)));
        assert_eq!(parse_pactl_volume_and_mute(vol, Some("Mute: yes\n")), Some((50, true)));
        assert_eq!(parse_pactl_volume_and_mute("invalid output", None), None);
    }

    #[test]
    fn parse_pactl_sinks_parses_and_sorts_default_first() {
        let sample = "\
Sink #41
\tState: RUNNING
\tName: alsa_output.pci-0000_00_1f.3.analog-stereo
\tDescription: Built-in Audio Analog Stereo
\tVolume: front-left: 40000 /  61%

Sink #52
\tState: IDLE
\tName: bluez_output.AA_BB_CC_DD_EE_FF.1
\tDescription: Pixel Buds Pro
";
        let devices = parse_pactl_sinks(sample, Some("bluez_output.AA_BB_CC_DD_EE_FF.1"));
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, "bluez_output.AA_BB_CC_DD_EE_FF.1");
        assert_eq!(devices[0].name, "Pixel Buds Pro");
        assert!(devices[0].is_default);
        assert_eq!(devices[1].id, "alsa_output.pci-0000_00_1f.3.analog-stereo");
        assert_eq!(devices[1].name, "Built-in Audio Analog Stereo");
        assert!(!devices[1].is_default);
    }

    #[test]
    fn parse_wpctl_status_sinks_parses_pipewire_tree_and_resolves_node_id() {
        let status = "\
PipeWire 'pipewire-0' [1.0.5, user@host, cookie:1234]
 └─ Clients:
        32. WirePlumber                         [1.0.5, user@host, pid:999]

Audio
 ├─ Devices:
 │      42. Built-in Audio                      [alsa]
 │
 ├─ Sinks:
 │      46. Built-in Audio Analog Stereo        [vol: 0.45]
 │  *   58. Pixel Buds Pro                      [vol: 0.80 MUTED]
 │
 ├─ Sources:
 │  *   47. Built-in Audio Analog Stereo        [vol: 1.00]
";
        let status_name = "\
Audio
 ├─ Sinks:
 │      46. alsa_output.pci-0000_00_1f.3.analog-stereo [vol: 0.45]
 │  *   58. bluez_output.AA_BB_CC_DD_EE_FF.1    [vol: 0.80 MUTED]
 ├─ Sources:
";
        let devices = parse_wpctl_status_sinks(status);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, "58");
        assert_eq!(devices[0].name, "Pixel Buds Pro");
        assert!(devices[0].is_default);
        assert_eq!(devices[1].id, "46");
        assert_eq!(devices[1].name, "Built-in Audio Analog Stereo");
        assert!(!devices[1].is_default);

        assert_eq!(resolve_wpctl_sink_node_id(status, Some(status_name), "58").as_deref(), Some("58"));
        assert_eq!(
            resolve_wpctl_sink_node_id(status, Some(status_name), "Built-in Audio Analog Stereo").as_deref(),
            Some("46")
        );
        assert_eq!(
            resolve_wpctl_sink_node_id(
                status,
                Some(status_name),
                "alsa_output.pci-0000_00_1f.3.analog-stereo"
            )
            .as_deref(),
            Some("46")
        );
        assert!(resolve_wpctl_sink_node_id(status, Some(status_name), "nonexistent").is_none());
    }

    #[test]
    fn parse_pactl_subscribe_line_filters_relevant_events() {
        assert!(parse_pactl_subscribe_line("Event 'change' on sink #0"));
        assert!(parse_pactl_subscribe_line("Event 'change' on source #2"));
        assert!(parse_pactl_subscribe_line("Event 'change' on server #0"));
        assert!(!parse_pactl_subscribe_line("Event 'change' on sink-input #44"));
        assert!(!parse_pactl_subscribe_line("Event 'change' on source-output #12"));
        assert!(!parse_pactl_subscribe_line("Event 'new' on client #99"));
    }

    #[test]
    fn volume_stepping_increments_by_two_percent_and_unmutes_on_up() {
        let _ = set_speaker_volume(40);
        let _ = set_speaker_mute(true);
        let (vol_up, muted_after_up) = speaker_volume_up().unwrap();
        assert!(!muted_after_up);
        assert!(vol_up <= 100);

        let (vol_down, _) = speaker_volume_down().unwrap();
        assert!(vol_down <= 100);
    }
}
