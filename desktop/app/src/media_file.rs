// SPDX-License-Identifier: GPL-3.0-or-later
//! Play on phone for video files (docs/protocol/media.md §4): which file a
//! desktop player on this PC plays. Players don't say, but Windows keeps a
//! shortcut to every file opened lately (the Recent folder), and the
//! player's title or its window's names the file.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

use crate::win::{browser, shortcut};

/// Formats a phone's own player plays.
const PLAYABLE: &[&str] = &["mp4", "m4v", "mkv", "webm", "mov", "3gp", "ts"];

/// How many of the latest Recent shortcuts to look through.
const RECENT: usize = 400;

/// Whether a phone plays a file of this name itself.
pub fn playable(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PLAYABLE.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

/// Letters and digits only, lowercase: "Holiday (2024).mp4 - VLC" and
/// "holiday_2024" compare alike.
fn words(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// How well `title` names the file `file_name`: 2 when it is just its
/// name, 1 when one holds the other, 0 when it doesn't name it.
pub(crate) fn names(title: &str, file_name: &str) -> u8 {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or(file_name);
    let (title, stem) = (words(title), words(stem));
    if title.chars().count() < 4 || stem.chars().count() < 4 {
        0
    } else if title == stem {
        2
    } else if title.contains(&stem) || stem.contains(&title) {
        1
    } else {
        0
    }
}

/// The file among `recent` (file names, newest first) that `titles` name
/// best: just its name, else the longest name a title holds; the newest of
/// equals.
pub(crate) fn pick<'a>(titles: &[&str], recent: &'a [String]) -> Option<&'a String> {
    let mut best: Option<((u8, usize), &String)> = None;
    for name in recent.iter().filter(|n| playable(n)) {
        let level = titles.iter().map(|t| names(t, name)).max().unwrap_or(0);
        if level == 0 {
            continue;
        }
        let score = (level, words(name).len());
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, name));
        }
    }
    best.map(|(_, name)| name)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn percent_decode_str(input: &str) -> String {
    let unescaped =
        input.replace("&amp;", "&").replace("&apos;", "'").replace("&#39;", "'").replace("&quot;", "\"");
    let bytes = unescaped.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) =
                ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16))
        {
            out.push(((hi << 4) | lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolves a `file://` URI (with percent-encoding and optional XML entity escaping)
/// into a local filesystem [`PathBuf`].
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn decode_file_uri(uri: &str) -> Option<PathBuf> {
    let trimmed = uri.trim().trim_matches('\'').trim_matches('"').trim();
    let after_scheme = trimmed.strip_prefix("file://")?;
    let path_part = after_scheme
        .strip_prefix("localhost/")
        .map(|r| format!("/{r}"))
        .unwrap_or_else(|| after_scheme.to_owned());
    let decoded = percent_decode_str(&path_part);
    if decoded.trim().is_empty() { None } else { Some(PathBuf::from(decoded)) }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn extract_xml_attr<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    for quote in ['"', '\''] {
        let pattern = format!("{attr}={quote}");
        if let Some(start) = tag.find(&pattern) {
            let rest = &tag[start + pattern.len()..];
            if let Some(end) = rest.find(quote) {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

/// Parses Freedesktop `recently-used.xbel` XML content and returns playable video
/// file paths ordered from most recently visited/modified to oldest.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_xbel_recent_files(xml: &str) -> Vec<PathBuf> {
    let mut entries: Vec<(String, usize, PathBuf)> = Vec::new();
    for (idx, chunk) in xml.split("<bookmark").skip(1).enumerate() {
        let Some(tag_end) = chunk.find('>') else {
            continue;
        };
        let tag = &chunk[..tag_end];
        let Some(href) = extract_xml_attr(tag, "href") else {
            continue;
        };
        let Some(path) = decode_file_uri(href) else {
            continue;
        };
        let is_playable = path.file_name().and_then(|n| n.to_str()).is_some_and(playable);
        if !is_playable {
            continue;
        }
        let timestamp = extract_xml_attr(tag, "visited")
            .or_else(|| extract_xml_attr(tag, "modified"))
            .or_else(|| extract_xml_attr(tag, "added"))
            .unwrap_or("")
            .to_owned();
        entries.push((timestamp, idx, path));
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut out = Vec::with_capacity(entries.len().min(RECENT));
    for (_, _, path) in entries {
        if !out.contains(&path) {
            out.push(path);
            if out.len() >= RECENT {
                break;
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn mpris_xesam_url_for(player: &str, title: &str) -> Option<PathBuf> {
    use std::collections::HashMap;
    use zbus::zvariant::OwnedValue;

    let conn = zbus::blocking::Connection::session().ok()?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .ok()?;
    let bus_names: Vec<String> = reply
        .body()
        .deserialize::<Vec<String>>()
        .ok()
        .or_else(|| reply.body().deserialize::<(Vec<String>,)>().ok().map(|(v,)| v))?;

    let player_lower = player.trim().to_ascii_lowercase();
    for bus in bus_names {
        let Some(short) = bus.strip_prefix("org.mpris.MediaPlayer2.") else {
            continue;
        };
        let short_lower = short.to_ascii_lowercase();
        if !player_lower.is_empty()
            && !short_lower.contains(&player_lower)
            && !player_lower.contains(&short_lower)
        {
            continue;
        }
        let Ok(msg) = conn.call_method(
            Some(bus.as_str()),
            "/org/mpris/MediaPlayer2",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.mpris.MediaPlayer2.Player", "Metadata"),
        ) else {
            continue;
        };
        let meta_val = msg
            .body()
            .deserialize::<OwnedValue>()
            .ok()
            .or_else(|| msg.body().deserialize::<(OwnedValue,)>().ok().map(|(v,)| v));
        let Some(meta_dict) = meta_val
            .and_then(|v| v.try_clone().ok())
            .and_then(|owned| HashMap::<String, OwnedValue>::try_from(owned).ok())
        else {
            continue;
        };
        let Some(url_str) = meta_dict.get("xesam:url").and_then(|v| <&str>::try_from(v).ok()) else {
            continue;
        };
        let Some(path) = decode_file_uri(url_str) else {
            continue;
        };
        if path.is_file() && path.file_name().and_then(|n| n.to_str()).is_some_and(playable) {
            if title.trim().is_empty() {
                return Some(path);
            }
            if let Some(fname) = path.file_name().and_then(|n| n.to_str())
                && names(title, fname) > 0
            {
                return Some(path);
            }
            return Some(path);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn proc_open_playable_files(programs: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(proc_dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in proc_dir.flatten() {
        let pid_os = entry.file_name();
        let Some(pid_str) = pid_os.to_str() else {
            continue;
        };
        if !pid_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm_path = entry.path().join("comm");
        let comm = std::fs::read_to_string(&comm_path).unwrap_or_default().trim().to_ascii_lowercase();
        if comm.is_empty() || !programs.iter().any(|p| p.eq_ignore_ascii_case(&comm) || comm.contains(p)) {
            continue;
        }
        let Ok(fd_dir) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd_entry in fd_dir.flatten() {
            if let Ok(target) = std::fs::read_link(fd_entry.path())
                && target.is_file()
                && target.file_name().and_then(|n| n.to_str()).is_some_and(playable)
                && !out.contains(&target)
            {
                out.push(target);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn xbel_recent_playable_files() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(data_dir) = dirs::data_local_dir() {
        candidates.push(data_dir.join("recently-used.xbel"));
    }
    if let Some(home) = dirs::home_dir() {
        let fallback = home.join(".local/share/recently-used.xbel");
        if !candidates.contains(&fallback) {
            candidates.push(fallback);
        }
    }
    for xbel_path in candidates {
        if let Ok(xml) = std::fs::read_to_string(&xbel_path) {
            let parsed = parse_xbel_recent_files(&xml);
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }
    Vec::new()
}

/// The video file this PC's `player` plays (`title`, as its media session
/// says), if it is one a phone plays and the system lists it as opened lately.
pub fn file_for(player: &str, title: &str) -> Option<PathBuf> {
    let programs = browser::programs_for(player);
    if programs.iter().any(|p| browser::is_browser(p)) {
        return None;
    }

    #[cfg(target_os = "linux")]
    if let Some(path) = mpris_xesam_url_for(player, title) {
        return Some(path);
    }

    let windows: Vec<String> =
        browser::windows().into_iter().filter(|w| programs.contains(&w.program)).map(|w| w.title).collect();
    let mut titles: Vec<&str> = windows.iter().map(String::as_str).collect();
    titles.push(title);

    #[cfg(target_os = "linux")]
    {
        let open_files = proc_open_playable_files(&programs);
        let open_names: Vec<String> = open_files
            .iter()
            .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(str::to_owned))
            .collect();
        if let Some(chosen) = pick(&titles, &open_names)
            && let Some(found) = open_files
                .into_iter()
                .find(|p| p.file_name().and_then(|s| s.to_str()) == Some(chosen.as_str()))
        {
            return Some(found);
        }

        let xbel_files: Vec<PathBuf> =
            xbel_recent_playable_files().into_iter().filter(|p| p.is_file()).collect();
        let xbel_names: Vec<String> = xbel_files
            .iter()
            .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(str::to_owned))
            .collect();
        if let Some(chosen) = pick(&titles, &xbel_names)
            && let Some(found) = xbel_files
                .into_iter()
                .find(|p| p.file_name().and_then(|s| s.to_str()) == Some(chosen.as_str()))
        {
            return Some(found);
        }
    }

    let dir = shortcut::recent_dir()?;
    let mut links: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    links.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    links.truncate(RECENT);
    // "Holiday.mp4.lnk" is the shortcut to Holiday.mp4.
    let names: Vec<String> =
        links.iter().filter_map(|(_, p)| p.file_stem().and_then(|s| s.to_str()).map(str::to_owned)).collect();
    let name = pick(&titles, &names)?;
    let link = links.iter().find(|(_, p)| p.file_stem().and_then(|s| s.to_str()) == Some(name.as_str()))?;
    let target = shortcut::target(&link.1)?;
    (target.is_file() && target.file_name().and_then(|n| n.to_str()).is_some_and(playable)).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_formats_phones_play() {
        assert!(playable("Holiday.MP4") && playable("a.mkv") && playable("clip.webm"));
        assert!(!playable("old.avi") && !playable("song.mp3") && !playable("notes.txt") && !playable("mp4"));
    }

    #[test]
    fn a_title_names_its_file() {
        assert_eq!(names("Holiday 2024", "Holiday_2024.mp4"), 2);
        assert_eq!(names("Holiday_2024.mp4 - VLC media player", "Holiday_2024.mp4"), 1);
        assert_eq!(names("Holiday", "Holiday 2024 (director's cut).mkv"), 1);
        assert_eq!(names("Something else", "Holiday.mp4"), 0);
        assert_eq!(names("abc", "abc.mp4"), 0, "too short to tell");
    }

    #[test]
    fn the_best_named_playable_file_wins_and_newest_breaks_ties() {
        let recent: Vec<String> =
            ["Holiday 2024.mkv", "Holiday 2024 extras.mp4", "Holiday 2024.avi", "Holiday 2024.mp4"]
                .map(String::from)
                .to_vec();
        // Exactly named beats only contained; the .avi doesn't play on a phone.
        assert_eq!(pick(&["Holiday 2024"], &recent).map(String::as_str), Some("Holiday 2024.mkv"));
        assert_eq!(pick(&["Unrelated"], &recent), None);
        // The window's title counts too.
        let titles = ["", "Holiday 2024 extras.mp4 - VLC media player"];
        assert_eq!(pick(&titles, &recent).map(String::as_str), Some("Holiday 2024 extras.mp4"));
    }

    #[test]
    fn parses_xbel_recent_files_and_decodes_file_uris() {
        assert_eq!(
            decode_file_uri("file:///home/user/Videos/Director&apos;s%20Cut%20(2026).mkv"),
            Some(PathBuf::from("/home/user/Videos/Director's Cut (2026).mkv"))
        );

        let xbel = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0">
  <bookmark href="file:///home/user/Documents/notes.txt" visited="2026-10-08T10:00:00Z"/>
  <bookmark href="file:///home/user/Videos/Older%20Clip.mp4" visited="2026-10-07T09:00:00Z"/>
  <bookmark href="file:///home/user/Videos/Newest%20Movie%202026.mkv" visited="2026-10-08T12:30:00Z"/>
</xbel>"#;
        let files = parse_xbel_recent_files(xbel);
        assert_eq!(
            files,
            vec![
                PathBuf::from("/home/user/Videos/Newest Movie 2026.mkv"),
                PathBuf::from("/home/user/Videos/Older Clip.mp4"),
            ]
        );
    }
}
