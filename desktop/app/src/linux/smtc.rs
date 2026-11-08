// SPDX-License-Identifier: GPL-3.0-or-later
//! What plays on a phone, exported to Linux desktop media controls (MPRIS2
//! D-Bus service `org.mpris.MediaPlayer2.nectarlink` on `/org/mpris/MediaPlayer2`
//! and atomic JSON state snapshot).
//!
//! Mirrors the public API of `win::smtc`: [`start`] registers the command
//! callback and spawns the `"media-controls"` worker thread; [`show`] updates
//! or clears the currently displayed phone media session.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};

use nectarlink_core::{DeviceId, MediaAction};
use serde::{Deserialize, Serialize};
use zbus::zvariant::{ObjectPath, OwnedValue, Str, Value};

use crate::state::PlayerView;

/// What the controls show: one player on one device.
#[derive(Debug, Clone)]
pub struct Shown {
    pub device: DeviceId,
    pub device_name: String,
    pub view: PlayerView,
}

type OnCommand = Box<dyn Fn(DeviceId, String, MediaAction, Option<u64>) + Send + Sync>;

static ON_COMMAND: OnceLock<Mutex<Option<OnCommand>>> = OnceLock::new();
static QUEUE: OnceLock<Mutex<mpsc::Sender<Option<Shown>>>> = OnceLock::new();
static CURRENT: Mutex<Option<(DeviceId, String)>> = Mutex::new(None);
static LAST_SHOWN: Mutex<Option<Shown>> = Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MprisExportState {
    pub bus_name: String,
    pub enabled: bool,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub player_id: Option<String>,
    pub playback_status: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    pub length_us: Option<u64>,
    pub position_us: Option<u64>,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn on_command_slot() -> &'static Mutex<Option<OnCommand>> {
    ON_COMMAND.get_or_init(|| Mutex::new(None))
}

/// Formats the second line of the media flyout with source attribution matching `win::smtc`.
pub(crate) fn format_artist_line(artist: Option<&str>, app: &str, device_name: &str) -> String {
    let source = format!("{app} on {device_name}");
    match artist.map(str::trim).filter(|s| !s.is_empty()) {
        Some(artist) => format!("{artist} · {source}"),
        None => source,
    }
}

/// Starts the Linux media controls provider and spawns the `"media-controls"` worker thread.
pub fn start<F>(on_command: F)
where
    F: Fn(DeviceId, String, MediaAction, Option<u64>) + Send + Sync + 'static,
{
    *lock(on_command_slot()) = Some(Box::new(on_command));
    if QUEUE.get().is_some() {
        return;
    }
    let (sender, receiver) = mpsc::channel::<Option<Shown>>();
    if QUEUE.set(Mutex::new(sender)).is_ok() {
        let spawned =
            std::thread::Builder::new().name("media-controls".into()).spawn(move || run_worker(receiver));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "Linux media-controls worker unavailable");
        }
    }
}

fn current_export_state() -> MprisExportState {
    let shown = lock(&LAST_SHOWN).clone();
    build_export_state(shown.as_ref())
}

pub(crate) fn build_mpris_metadata_map(state: &MprisExportState) -> HashMap<String, OwnedValue> {
    let mut map = HashMap::new();
    let track_path = if state.enabled {
        "/org/nectarlink/MediaPlayer2/Track/current"
    } else {
        "/org/mpris/MediaPlayer2/TrackList/NoTrack"
    };
    if let Ok(path) = ObjectPath::try_from(track_path)
        && let Ok(val) = OwnedValue::try_from(Value::from(path))
    {
        map.insert("mpris:trackid".to_owned(), val);
    }
    if !state.enabled {
        return map;
    }
    if let Ok(val) = OwnedValue::try_from(Value::from(Str::from(state.title.as_str()))) {
        map.insert("xesam:title".to_owned(), val);
    }
    let artists: Vec<Str<'_>> =
        if state.artist.trim().is_empty() { Vec::new() } else { vec![Str::from(state.artist.as_str())] };
    if let Ok(val) = OwnedValue::try_from(Value::from(artists)) {
        map.insert("xesam:artist".to_owned(), val);
    }
    if let Ok(val) = OwnedValue::try_from(Value::from(Str::from(state.album.as_str()))) {
        map.insert("xesam:album".to_owned(), val);
    }
    if let Some(length_us) = state.length_us {
        let len_i64 = i64::try_from(length_us).unwrap_or(i64::MAX);
        map.insert("mpris:length".to_owned(), OwnedValue::from(len_i64));
    }
    if let Some(art_url) = &state.art_url
        && let Ok(val) = OwnedValue::try_from(Value::from(Str::from(art_url.as_str())))
    {
        map.insert("mpris:artUrl".to_owned(), val);
    }
    map
}

pub(crate) fn build_mpris_player_properties(state: &MprisExportState) -> HashMap<String, OwnedValue> {
    let mut props = HashMap::new();
    if let Ok(val) = OwnedValue::try_from(Value::from(Str::from(state.playback_status.as_str()))) {
        props.insert("PlaybackStatus".to_owned(), val);
    }
    if let Ok(meta_val) = OwnedValue::try_from(Value::from(build_mpris_metadata_map(state))) {
        props.insert("Metadata".to_owned(), meta_val);
    }
    let pos_i64 = state.position_us.and_then(|u| i64::try_from(u).ok()).unwrap_or(0);
    props.insert("Position".to_owned(), OwnedValue::from(pos_i64));
    props.insert("CanPlay".to_owned(), OwnedValue::from(state.can_play));
    props.insert("CanPause".to_owned(), OwnedValue::from(state.can_pause));
    props.insert("CanGoNext".to_owned(), OwnedValue::from(state.can_go_next));
    props.insert("CanGoPrevious".to_owned(), OwnedValue::from(state.can_go_previous));
    props.insert("CanSeek".to_owned(), OwnedValue::from(state.can_seek));
    props.insert("CanControl".to_owned(), OwnedValue::from(state.enabled));
    props
}

struct MprisRoot;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl MprisRoot {
    fn raise(&self) {}

    fn quit(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> String {
        "NectarLink".to_owned()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        "nectarlink".to_owned()
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct MprisPlayer;

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl MprisPlayer {
    fn next(&self) {
        let _ = handle_mpris_call("Next", None);
    }

    fn previous(&self) {
        let _ = handle_mpris_call("Previous", None);
    }

    fn pause(&self) {
        let _ = handle_mpris_call("Pause", None);
    }

    fn play_pause(&self) {
        let _ = handle_mpris_call("PlayPause", None);
    }

    fn stop(&self) {
        let _ = handle_mpris_call("Stop", None);
    }

    fn play(&self) {
        let _ = handle_mpris_call("Play", None);
    }

    fn seek(&self, offset: i64) {
        let _ = handle_mpris_seek_offset(offset);
    }

    fn set_position(&self, _track_id: ObjectPath<'_>, position: i64) {
        let _ = handle_mpris_call("SetPosition", Some(position));
    }

    fn open_uri(&self, _uri: &str) {}

    #[zbus(property)]
    fn playback_status(&self) -> String {
        current_export_state().playback_status
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        build_mpris_metadata_map(&current_export_state())
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        current_export_state().position_us.and_then(|u| i64::try_from(u).ok()).unwrap_or(0)
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        current_export_state().can_go_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        current_export_state().can_go_previous
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        current_export_state().can_play
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        current_export_state().can_pause
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        current_export_state().can_seek
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        current_export_state().enabled
    }
}

fn init_mpris_dbus_service() -> Option<zbus::blocking::Connection> {
    let builder = zbus::blocking::connection::Builder::session().ok()?;
    let builder = builder.name("org.mpris.MediaPlayer2.nectarlink").ok()?;
    let builder = builder.serve_at("/org/mpris/MediaPlayer2", MprisRoot).ok()?;
    let builder = builder.serve_at("/org/mpris/MediaPlayer2", MprisPlayer).ok()?;
    builder.build().ok()
}

fn emit_mpris_properties_changed(conn: &zbus::blocking::Connection, state: &MprisExportState) {
    let changed = build_mpris_player_properties(state);
    let invalidated: Vec<&str> = Vec::new();
    let _ = conn.emit_signal(
        None::<&str>,
        "/org/mpris/MediaPlayer2",
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &("org.mpris.MediaPlayer2.Player", changed, invalidated),
    );
}

fn run_worker(receiver: mpsc::Receiver<Option<Shown>>) {
    let mut dbus_conn = if cfg!(test) { None } else { init_mpris_dbus_service() };
    while let Ok(first) = receiver.recv() {
        let mut latest = first;
        while let Ok(next) = receiver.try_recv() {
            latest = next;
        }
        let export = build_export_state(latest.as_ref());
        write_mpris_state_file(&export);
        if !cfg!(test) {
            if dbus_conn.is_none() {
                dbus_conn = init_mpris_dbus_service();
            }
            if let Some(conn) = &dbus_conn {
                emit_mpris_properties_changed(conn, &export);
            }
        }
    }
}

/// Shows a phone player in the desktop media controls, or hides them when `None`.
pub fn show(current: Option<Shown>) {
    match &current {
        Some(shown) => {
            *lock(&CURRENT) = Some((shown.device, shown.view.player.id.clone()));
        }
        None => {
            *lock(&CURRENT) = None;
        }
    }
    *lock(&LAST_SHOWN) = current.clone();
    if let Some(queue) = QUEUE.get() {
        let _ = lock(queue).send(current);
    } else {
        let export = build_export_state(current.as_ref());
        write_mpris_state_file(&export);
    }
}

pub(crate) fn build_export_state(shown: Option<&Shown>) -> MprisExportState {
    let Some(shown) = shown else {
        return MprisExportState {
            bus_name: "org.mpris.MediaPlayer2.nectarlink".to_owned(),
            enabled: false,
            device_id: None,
            device_name: None,
            player_id: None,
            playback_status: "Stopped".to_owned(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            art_url: None,
            length_us: None,
            position_us: None,
            can_play: false,
            can_pause: false,
            can_go_next: false,
            can_go_previous: false,
            can_seek: false,
        };
    };

    let player = &shown.view.player;
    let can = |action: &str| player.actions.iter().any(|a| a == action);
    let title = player.title.as_deref().unwrap_or(&player.app).to_owned();
    let artist = format_artist_line(player.artist.as_deref(), &player.app, &shown.device_name);
    let album = player.album.clone().unwrap_or_default();
    let art_url = shown.view.art.as_ref().map(|p| format!("file://{}", p.display()));

    let elapsed_ms = if player.playing {
        SystemTime::now().duration_since(shown.view.at).map_or(0, |d| d.as_millis() as u64)
    } else {
        0
    };
    let duration_ms = player.duration;
    let position_ms = player.position.map(|pos| {
        let advanced = pos.saturating_add(elapsed_ms);
        duration_ms.map_or(advanced, |d| advanced.min(d))
    });

    MprisExportState {
        bus_name: "org.mpris.MediaPlayer2.nectarlink".to_owned(),
        enabled: true,
        device_id: Some(shown.device.to_string()),
        device_name: Some(shown.device_name.clone()),
        player_id: Some(player.id.clone()),
        playback_status: if player.playing { "Playing".to_owned() } else { "Paused".to_owned() },
        title,
        artist,
        album,
        art_url,
        length_us: duration_ms.map(|ms| ms.saturating_mul(1000)),
        position_us: position_ms.map(|ms| ms.saturating_mul(1000)),
        can_play: can("play"),
        can_pause: can("pause"),
        can_go_next: can("next"),
        can_go_previous: can("previous"),
        can_seek: can("seek"),
    }
}

fn mpris_state_path() -> Option<PathBuf> {
    if let Some(base) = dirs::runtime_dir().or_else(dirs::cache_dir).filter(|p| p.is_absolute()) {
        Some(base.join("nectarlink").join("mpris-state.json"))
    } else {
        let uid = unsafe { libc::geteuid() };
        Some(std::env::temp_dir().join(format!("nectarlink-{uid}")).join("mpris-state.json"))
    }
}

fn write_mpris_state_file(state: &MprisExportState) {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

    let Some(path) = mpris_state_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
    let euid = unsafe { libc::geteuid() };
    let Ok(meta) = fs::symlink_metadata(parent) else {
        return;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != euid {
        return;
    }
    let Ok(json) = serde_json::to_vec_pretty(state) else {
        return;
    };
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let tmp_path = parent.join(format!(".mpris-state.{}.{}.tmp", std::process::id(), nanos));
    let Ok(mut file) = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(&tmp_path)
    else {
        return;
    };
    if file.write_all(&json).is_ok() {
        let _ = fs::rename(&tmp_path, &path);
    } else {
        let _ = fs::remove_file(&tmp_path);
    }
}

/// Dispatches a [`MediaAction`] for the currently shown phone media session.
pub fn dispatch_command(action: MediaAction, position_ms: Option<u64>) -> bool {
    let Some((device, player_id)) = lock(&CURRENT).clone() else {
        return false;
    };
    let guard = lock(on_command_slot());
    let Some(on_command) = guard.as_ref() else {
        return false;
    };
    on_command(device, player_id, action, position_ms);
    true
}

/// Handles an incoming MPRIS2 `org.mpris.MediaPlayer2.Player` method call.
pub fn handle_mpris_call(method: &str, position_us: Option<i64>) -> bool {
    match method.trim() {
        "Play" => dispatch_command(MediaAction::Play, None),
        "Pause" | "Stop" => dispatch_command(MediaAction::Pause, None),
        "PlayPause" => {
            let currently_playing = lock(&LAST_SHOWN).as_ref().is_some_and(|s| s.view.player.playing);
            let action = if currently_playing { MediaAction::Pause } else { MediaAction::Play };
            dispatch_command(action, None)
        }
        "Next" => dispatch_command(MediaAction::Next, None),
        "Previous" => dispatch_command(MediaAction::Previous, None),
        "Seek" | "SetPosition" => {
            let pos_ms = position_us.map(|us| u64::try_from((us / 1000).max(0)).unwrap_or(0));
            dispatch_command(MediaAction::Seek, pos_ms)
        }
        _ => false,
    }
}

/// Handles a relative MPRIS2 `Seek(Offset)` call by adding `offset_us` to the
/// currently exported playback position and dispatching `MediaAction::Seek`.
pub fn handle_mpris_seek_offset(offset_us: i64) -> bool {
    let export = current_export_state();
    let cur_us = export.position_us.and_then(|u| i64::try_from(u).ok()).unwrap_or(0);
    let mut target_us = cur_us.saturating_add(offset_us).max(0);
    if let Some(len_us) = export.length_us.and_then(|u| i64::try_from(u).ok()) {
        target_us = target_us.min(len_us);
    }
    let pos_ms = u64::try_from(target_us / 1000).unwrap_or(0);
    dispatch_command(MediaAction::Seek, Some(pos_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nectarlink_core::MediaPlayer;
    use std::sync::Arc;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn sample_shown(playing: bool) -> Shown {
        let device = DeviceId([7u8; 32]);
        Shown {
            device,
            device_name: "Pixel 9".to_owned(),
            view: PlayerView {
                device,
                player: MediaPlayer {
                    id: "com.spotify.music".to_owned(),
                    app: "Spotify".to_owned(),
                    title: Some("Veridis Quo".to_owned()),
                    artist: Some("Daft Punk".to_owned()),
                    album: Some("Discovery".to_owned()),
                    playing,
                    duration: Some(345_000),
                    position: Some(12_000),
                    actions: vec![
                        "play".to_owned(),
                        "pause".to_owned(),
                        "next".to_owned(),
                        "previous".to_owned(),
                        "seek".to_owned(),
                    ],
                    art_key: None,
                    art: None,
                },
                art: Some(PathBuf::from("/tmp/discovery.jpg")),
                at: SystemTime::now(),
            },
        }
    }

    #[test]
    fn formats_artist_line_with_source_attribution() {
        assert_eq!(
            format_artist_line(Some("Daft Punk"), "Spotify", "Pixel 9"),
            "Daft Punk · Spotify on Pixel 9"
        );
        assert_eq!(format_artist_line(None, "YouTube Music", "Galaxy S24"), "YouTube Music on Galaxy S24");
    }

    #[test]
    fn show_and_mpris_calls_dispatch_commands_and_clear_on_none() {
        type ReceivedMediaCmd = (DeviceId, String, MediaAction, Option<u64>);
        let _serial = lock(&TEST_LOCK);
        let received: Arc<Mutex<Vec<ReceivedMediaCmd>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        start(move |dev, player, action, pos| {
            lock(&sink).push((dev, player, action, pos));
        });

        let shown_playing = sample_shown(true);
        let expected_dev = shown_playing.device;
        show(Some(shown_playing));

        assert!(handle_mpris_call("Play", None));
        assert!(handle_mpris_call("Pause", None));
        // PlayPause while playing -> Pause
        assert!(handle_mpris_call("PlayPause", None));
        assert!(handle_mpris_call("Next", None));
        assert!(handle_mpris_call("Previous", None));
        assert!(handle_mpris_call("Seek", Some(42_500_000)));

        // Switch state to paused and test PlayPause -> Play
        show(Some(sample_shown(false)));
        assert!(handle_mpris_call("PlayPause", None));

        let events = lock(&received).clone();
        assert_eq!(events.len(), 7);
        assert_eq!(events[0], (expected_dev, "com.spotify.music".to_owned(), MediaAction::Play, None));
        assert_eq!(events[1], (expected_dev, "com.spotify.music".to_owned(), MediaAction::Pause, None));
        assert_eq!(events[2], (expected_dev, "com.spotify.music".to_owned(), MediaAction::Pause, None));
        assert_eq!(events[3], (expected_dev, "com.spotify.music".to_owned(), MediaAction::Next, None));
        assert_eq!(events[4], (expected_dev, "com.spotify.music".to_owned(), MediaAction::Previous, None));
        assert_eq!(
            events[5],
            (expected_dev, "com.spotify.music".to_owned(), MediaAction::Seek, Some(42_500))
        );
        assert_eq!(events[6], (expected_dev, "com.spotify.music".to_owned(), MediaAction::Play, None));

        // Clearing with show(None) stops dispatching commands
        show(None);
        assert!(!handle_mpris_call("Play", None));
        let cleared_state = build_export_state(None);
        assert!(!cleared_state.enabled);
        assert_eq!(cleared_state.playback_status, "Stopped");
    }

    #[test]
    fn builds_mpris_metadata_and_player_properties_dictionaries() {
        let shown = sample_shown(true);
        let export = build_export_state(Some(&shown));
        let meta = build_mpris_metadata_map(&export);
        assert!(meta.contains_key("mpris:trackid"));
        assert_eq!(meta.get("xesam:title").and_then(|v| <&str>::try_from(v).ok()), Some("Veridis Quo"));
        assert_eq!(meta.get("xesam:album").and_then(|v| <&str>::try_from(v).ok()), Some("Discovery"));
        assert_eq!(meta.get("mpris:length").and_then(|v| i64::try_from(v).ok()), Some(345_000_000));

        let props = build_mpris_player_properties(&export);
        assert_eq!(props.get("PlaybackStatus").and_then(|v| <&str>::try_from(v).ok()), Some("Playing"));
        assert_eq!(props.get("CanPlay").and_then(|v| bool::try_from(v).ok()), Some(true));
        assert_eq!(props.get("CanControl").and_then(|v| bool::try_from(v).ok()), Some(true));
    }
}
