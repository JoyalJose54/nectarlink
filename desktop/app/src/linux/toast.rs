// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone notifications as Freedesktop desktop notifications on Linux, with
//! action buttons, inline replies, progress updates, and D-Bus signal routing.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

pub const AUMID: &str = "Nectarlink.Desktop";
pub const CALL_ANSWER: &str = "answer";
pub const CALL_DECLINE: &str = "decline";
pub const CALL_END: &str = "end";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastEvent {
    Opened { device: String, key: String },
    Action { device: String, key: String, action: String },
    Reply { device: String, key: String, action: String, text: String },
    Dismissed { device: String, key: String },
}

#[derive(Clone)]
pub struct Toast {
    pub device: String,
    pub key: String,
    pub title: String,
    pub body: String,
    pub attribution: String,
    pub icon: Option<PathBuf>,
    pub image: Option<PathBuf>,
    pub actions: Vec<(String, String)>,
    pub reply: Option<(String, String)>,
    pub silent: bool,
    pub progress: Option<Progress>,
    pub call: bool,
}

impl fmt::Debug for Toast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Toast")
            .field("device", &self.device)
            .field("key", &self.key)
            .field("title", &"<redacted>")
            .field("title_len", &self.title.len())
            .field("body", &"<redacted>")
            .field("body_len", &self.body.len())
            .field("attribution", &self.attribution)
            .field("icon", &self.icon)
            .field("image", &self.image)
            .field("actions", &self.actions.len())
            .field("reply", &self.reply.is_some())
            .field("silent", &self.silent)
            .field("progress", &self.progress)
            .field("call", &self.call)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub status: String,
    pub value: Option<f64>,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorSignal {
    ActionInvoked { id: u32, action_key: String },
    NotificationReplied { id: u32, text: String },
    NotificationClosed { id: u32, reason: u32 },
}

#[derive(Clone)]
struct ToastSnapshot {
    title: String,
    body: String,
    attribution: String,
    icon_path: String,
    image_path: Option<String>,
    actions: Vec<(String, String)>,
    reply: Option<(String, String)>,
    silent: bool,
    call: bool,
}

impl fmt::Debug for ToastSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToastSnapshot")
            .field("title", &"<redacted>")
            .field("title_len", &self.title.len())
            .field("body", &"<redacted>")
            .field("body_len", &self.body.len())
            .field("attribution", &self.attribution)
            .field("icon_path", &self.icon_path)
            .field("image_path", &self.image_path)
            .field("actions", &self.actions.len())
            .field("reply", &self.reply.is_some())
            .field("silent", &self.silent)
            .field("call", &self.call)
            .finish()
    }
}

type EventCallback = Arc<dyn Fn(ToastEvent) + Send + Sync + 'static>;

struct Toaster {
    icon: PathBuf,
    on_event: Option<EventCallback>,
    active_ids: HashMap<(String, String), u32>,
    id_to_target: HashMap<u32, (String, String)>,
    reply_actions: HashMap<u32, String>,
    snapshots: HashMap<(String, String), ToastSnapshot>,
    next_fallback_id: u32,
}

impl Toaster {
    fn new(icon: PathBuf, on_event: Option<EventCallback>) -> Self {
        Self {
            icon,
            on_event,
            active_ids: HashMap::new(),
            id_to_target: HashMap::new(),
            reply_actions: HashMap::new(),
            snapshots: HashMap::new(),
            next_fallback_id: 100_000,
        }
    }

    fn record_shown(&mut self, device: String, key: String, id: u32, snapshot: ToastSnapshot) {
        let target = (device, key);
        if let Some(old_id) = self.active_ids.insert(target.clone(), id)
            && old_id != id
        {
            self.id_to_target.remove(&old_id);
            self.reply_actions.remove(&old_id);
        }
        self.id_to_target.insert(id, target.clone());
        if let Some((reply_action, _)) = &snapshot.reply {
            self.reply_actions.insert(id, reply_action.clone());
        } else {
            self.reply_actions.remove(&id);
        }
        self.snapshots.insert(target, snapshot);
    }

    fn take_by_target(&mut self, device: &str, key: &str) -> Option<u32> {
        let target = (device.to_string(), key.to_string());
        self.snapshots.remove(&target);
        if let Some(id) = self.active_ids.remove(&target) {
            self.id_to_target.remove(&id);
            self.reply_actions.remove(&id);
            Some(id)
        } else {
            None
        }
    }

    fn take_by_device(&mut self, device: &str) -> Vec<u32> {
        let keys: Vec<(String, String)> =
            self.active_ids.keys().filter(|(d, _)| d == device).cloned().collect();
        let mut ids = Vec::new();
        for target in keys {
            if let Some(id) = self.active_ids.remove(&target) {
                self.id_to_target.remove(&id);
                self.reply_actions.remove(&id);
                ids.push(id);
            }
            self.snapshots.remove(&target);
        }
        ids
    }

    fn clear_all(&mut self) -> Vec<u32> {
        let ids: Vec<u32> = self.active_ids.values().copied().collect();
        self.active_ids.clear();
        self.id_to_target.clear();
        self.reply_actions.clear();
        self.snapshots.clear();
        ids
    }

    fn handle_signal(&mut self, signal: MonitorSignal) -> Option<ToastEvent> {
        match signal {
            MonitorSignal::ActionInvoked { id, action_key } => {
                let (device, key) = self.id_to_target.get(&id)?.clone();
                if action_key == "default" || action_key.is_empty() {
                    Some(ToastEvent::Opened { device, key })
                } else {
                    Some(ToastEvent::Action { device, key, action: action_key })
                }
            }
            MonitorSignal::NotificationReplied { id, text } => {
                let (device, key) = self.id_to_target.get(&id)?.clone();
                let action =
                    self.reply_actions.get(&id).cloned().unwrap_or_else(|| "inline-reply".to_string());
                Some(ToastEvent::Reply { device, key, action, text })
            }
            MonitorSignal::NotificationClosed { id, reason } => {
                let (device, key) = self.id_to_target.remove(&id)?;
                self.active_ids.remove(&(device.clone(), key.clone()));
                self.reply_actions.remove(&id);
                self.snapshots.remove(&(device.clone(), key.clone()));
                // Freedesktop NotificationClosed reasons:
                // 1 = expired, 2 = dismissed by user, 3 = closed by CloseNotification, 4 = undefined
                if reason == 2 { Some(ToastEvent::Dismissed { device, key }) } else { None }
            }
        }
    }
}

enum WorkerCommand {
    Show(Box<Toast>),
    UpdateProgress { device: String, key: String, progress: Progress },
    Remove { device: String, key: String },
    RemoveDevice { device: String },
}

static TOASTER: OnceLock<Mutex<Toaster>> = OnceLock::new();
static WORKER_TX: OnceLock<mpsc::Sender<WorkerCommand>> = OnceLock::new();
static MONITOR_STARTED: OnceLock<()> = OnceLock::new();
static DBUS_SESSION: OnceLock<Mutex<Option<zbus::blocking::Connection>>> = OnceLock::new();

/// Returns the shared D-Bus session connection so `Notify`, `CloseNotification`,
/// and `monitor_loop()` all share the same unique bus name (`:1.x`).
fn dbus_session() -> Option<zbus::blocking::Connection> {
    let mutex = DBUS_SESSION.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().ok()?;
    if guard.is_none() {
        *guard = zbus::blocking::Connection::session().ok();
    }
    guard.clone()
}

fn reset_dbus_session() -> Option<zbus::blocking::Connection> {
    let mutex = DBUS_SESSION.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().ok()?;
    *guard = zbus::blocking::Connection::session().ok();
    // A new connection may reach a different notification server.
    *CAPABILITIES.lock().unwrap_or_else(|e| e.into_inner()) = None;
    guard.clone()
}

/// What the notification server says it can show (`GetCapabilities`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Capabilities {
    /// Bold, italics and entities in the body; without it, markup shows as text.
    pub body_markup: bool,
    /// Buttons, including the default action when the notification is clicked.
    pub actions: bool,
    /// A reply field in the notification (KDE Plasma).
    pub inline_reply: bool,
}

impl Capabilities {
    pub(crate) fn from_list(list: &[String]) -> Self {
        let has = |name: &str| list.iter().any(|c| c == name);
        Capabilities {
            body_markup: has("body-markup"),
            actions: has("actions"),
            inline_reply: has("inline-reply"),
        }
    }
}

static CAPABILITIES: Mutex<Option<Capabilities>> = Mutex::new(None);

/// The server's capabilities, asked once per connection. When there is no
/// server to ask, plain text and no buttons.
fn capabilities() -> Capabilities {
    if let Some(caps) = *CAPABILITIES.lock().unwrap_or_else(|e| e.into_inner()) {
        return caps;
    }
    let Some(list) = with_session_connection(|conn| {
        let msg = conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "GetCapabilities",
            &(),
        )?;
        msg.body().deserialize::<Vec<String>>()
    }) else {
        return Capabilities::default();
    };
    let caps = Capabilities::from_list(&list);
    *CAPABILITIES.lock().unwrap_or_else(|e| e.into_inner()) = Some(caps);
    caps
}

fn with_session_connection<T, F>(mut op: F) -> Option<T>
where
    T: Send,
    F: FnMut(&zbus::blocking::Connection) -> zbus::Result<T> + Send,
{
    super::non_tokio(|| {
        let conn = dbus_session()?;
        if !super::dbus_name_has_owner(&conn, "org.freedesktop.Notifications") {
            return None;
        }
        match op(&conn) {
            Ok(val) => Some(val),
            Err(zbus::Error::InputOutput(_) | zbus::Error::Handshake(_)) => {
                let retry_conn = reset_dbus_session()?;
                if !super::dbus_name_has_owner(&retry_conn, "org.freedesktop.Notifications") {
                    return None;
                }
                op(&retry_conn).ok()
            }
            Err(_) => None,
        }
    })
}

/// Checks whether desktop notifications are currently enabled (not inhibited via
/// `org.freedesktop.Notifications` `Inhibited` property and not disabled in GNOME's
/// `org.gnome.desktop.notifications` `show-banners`).
pub fn enabled() -> bool {
    if cfg!(test) {
        return true;
    }
    // Asked each time the window comes to the front, on the GUI thread: keep
    // the answer for a while instead of asking D-Bus and gsettings every time.
    static CACHED: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);
    const KEEP: Duration = Duration::from_secs(30);
    if let Some((at, value)) = *CACHED.lock().unwrap_or_else(|e| e.into_inner())
        && at.elapsed() < KEEP
    {
        return value;
    }
    let value = query_enabled();
    *CACHED.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), value));
    value
}

fn query_enabled() -> bool {
    if let Some(inhibited) = with_session_connection(|conn| {
        let msg = conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.Notifications", "Inhibited"),
        )?;
        let body = msg.body();
        if let Ok(val) = body.deserialize::<zbus::zvariant::OwnedValue>()
            && let Ok(b) = bool::try_from(val)
        {
            return Ok(b);
        }
        if let Ok((val,)) = body.deserialize::<(zbus::zvariant::OwnedValue,)>()
            && let Ok(b) = bool::try_from(val)
        {
            return Ok(b);
        }
        Ok(false)
    }) && inhibited
    {
        return false;
    }

    let is_gnome = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .map(|d| d.to_ascii_lowercase().contains("gnome"))
        .unwrap_or(false);
    if is_gnome
        && let Ok(output) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.notifications", "show-banners"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        && output.status.success()
        && String::from_utf8_lossy(&output.stdout).trim() == "false"
    {
        return false;
    }

    true
}

pub fn set_process_id() {}

pub fn start<F>(icon: PathBuf, on_event: F)
where
    F: Fn(ToastEvent) + Send + Sync + 'static,
{
    if cfg!(test) {
        if !icon.exists() {
            if let Some(parent) = icon.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = super::icon::write_png(&icon, 64);
        }
    } else {
        let bg_icon = icon.clone();
        let _ = std::thread::Builder::new().name("toast-icon".into()).spawn(move || {
            if !bg_icon.exists() {
                if let Some(parent) = bg_icon.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = super::icon::write_png(&bg_icon, 128);
            }
        });
    }

    let callback: EventCallback = Arc::new(on_event);
    if let Some(mutex) = TOASTER.get() {
        if let Ok(mut guard) = mutex.lock() {
            guard.icon = icon;
            guard.on_event = Some(callback);
        }
    } else {
        let _ = TOASTER.set(Mutex::new(Toaster::new(icon, Some(callback))));
    }

    if !cfg!(test) {
        WORKER_TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            let _ = std::thread::Builder::new().name("toasts-worker".into()).spawn(move || worker_loop(rx));
            tx
        });
        MONITOR_STARTED.get_or_init(|| {
            let _ = std::thread::Builder::new().name("toasts-monitor".into()).spawn(monitor_loop);
        });
    }
    crate::pc_notifications::start_listener();
}

fn dispatch(cmd: WorkerCommand) {
    if !cfg!(test)
        && let Some(tx) = WORKER_TX.get()
    {
        match tx.send(cmd) {
            Ok(()) => return,
            Err(e) => {
                execute_command(e.0);
                return;
            }
        }
    }
    execute_command(cmd);
}

fn worker_loop(rx: mpsc::Receiver<WorkerCommand>) {
    while let Ok(cmd) = rx.recv() {
        execute_command(cmd);
    }
}

fn execute_command(cmd: WorkerCommand) {
    match cmd {
        WorkerCommand::Show(toast) => execute_show(*toast),
        WorkerCommand::UpdateProgress { device, key, progress } => {
            execute_update_progress(&device, &key, progress);
        }
        WorkerCommand::Remove { device, key } => execute_remove(&device, &key),
        WorkerCommand::RemoveDevice { device } => execute_remove_device(&device),
    }
}

pub fn show(toast: Toast) {
    if toast.device.trim().is_empty() || toast.key.trim().is_empty() {
        return;
    }
    dispatch(WorkerCommand::Show(Box::new(toast)));
}

fn execute_show(toast: Toast) {
    let image_str = toast.image.as_ref().map(|p| p.to_string_lossy().to_string());
    let title = format_summary(&toast.title, &toast.attribution);
    let markup = capabilities().body_markup;
    let body = format_body(&toast.title, &toast.body, &toast.attribution, toast.progress.as_ref(), markup);

    let Some(mutex) = TOASTER.get() else {
        let icon_str = toast
            .icon
            .as_ref()
            .or(toast.image.as_ref())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let _ = send_notification(
            0,
            &icon_str,
            image_str.as_deref(),
            &title,
            &body,
            &toast.actions,
            toast.reply.as_ref(),
            toast.progress.as_ref(),
            toast.silent,
            toast.call,
        );
        return;
    };

    // Hold the TOASTER lock across `send_notification` and `record_shown` on the worker
    // thread so a fast `ActionInvoked` or `NotificationClosed` signal in `monitor_loop()`
    // can never race ahead of `record_shown`.
    let Ok(mut guard) = mutex.lock() else {
        return;
    };
    let replaces_id = guard.active_ids.get(&(toast.device.clone(), toast.key.clone())).copied().unwrap_or(0);
    let chosen_icon =
        toast.icon.clone().or_else(|| toast.image.clone()).unwrap_or_else(|| guard.icon.clone());
    let icon_str = chosen_icon.to_string_lossy().to_string();

    let notified_id = send_notification(
        replaces_id,
        &icon_str,
        image_str.as_deref(),
        &title,
        &body,
        &toast.actions,
        toast.reply.as_ref(),
        toast.progress.as_ref(),
        toast.silent,
        toast.call,
    );

    let id = notified_id.unwrap_or_else(|| {
        if replaces_id != 0 {
            replaces_id
        } else {
            guard.next_fallback_id += 1;
            guard.next_fallback_id
        }
    });
    let snapshot = ToastSnapshot {
        title: toast.title,
        body: toast.body,
        attribution: toast.attribution,
        icon_path: icon_str,
        image_path: image_str,
        actions: toast.actions,
        reply: toast.reply,
        silent: toast.silent,
        call: toast.call,
    };
    guard.record_shown(toast.device, toast.key, id, snapshot);
}

pub fn update_progress(device: &str, key: &str, progress: Progress) {
    dispatch(WorkerCommand::UpdateProgress { device: device.to_string(), key: key.to_string(), progress });
}

fn execute_update_progress(device: &str, key: &str, progress: Progress) {
    let Some(mutex) = TOASTER.get() else {
        return;
    };
    let Ok(mut guard) = mutex.lock() else {
        return;
    };
    let target = (device.to_string(), key.to_string());
    let Some(&replaces_id) = guard.active_ids.get(&target) else {
        return;
    };
    let Some(snapshot) = guard.snapshots.get(&target).cloned() else {
        return;
    };

    let summary = format_summary(&snapshot.title, &snapshot.attribution);
    let markup = capabilities().body_markup;
    let body = format_body(&snapshot.title, &snapshot.body, &snapshot.attribution, Some(&progress), markup);

    let new_id = send_notification(
        replaces_id,
        &snapshot.icon_path,
        snapshot.image_path.as_deref(),
        &summary,
        &body,
        &snapshot.actions,
        snapshot.reply.as_ref(),
        Some(&progress),
        true,
        snapshot.call,
    );

    if let Some(id) = new_id
        && id != replaces_id
    {
        guard.record_shown(device.to_string(), key.to_string(), id, snapshot);
    }
}

pub fn remove(device: &str, key: &str) {
    dispatch(WorkerCommand::Remove { device: device.to_string(), key: key.to_string() });
}

fn execute_remove(device: &str, key: &str) {
    let Some(mutex) = TOASTER.get() else {
        return;
    };
    let id = {
        let Ok(mut guard) = mutex.lock() else {
            return;
        };
        guard.take_by_target(device, key)
    };
    if let Some(id) = id {
        close_dbus_notification(id);
    }
}

pub fn remove_device(device: &str) {
    dispatch(WorkerCommand::RemoveDevice { device: device.to_string() });
}

fn execute_remove_device(device: &str) {
    let Some(mutex) = TOASTER.get() else {
        return;
    };
    let ids = {
        let Ok(mut guard) = mutex.lock() else {
            return;
        };
        guard.take_by_device(device)
    };
    for id in ids {
        close_dbus_notification(id);
    }
}

pub fn unregister() {
    execute_unregister();
}

fn execute_unregister() {
    let Some(mutex) = TOASTER.get() else {
        return;
    };
    let ids = {
        let Ok(mut guard) = mutex.lock() else {
            return;
        };
        guard.clear_all()
    };
    for id in ids {
        close_dbus_notification(id);
    }
}

/// Escapes XML/Pango entities (`&`, `<`, `>`) so notification body text cannot
/// break Pango markup parsing in Freedesktop notification daemons.
pub fn escape_notification_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// The summary line: the title, or where the notification came from when
/// there's no title (then [`format_body`] doesn't repeat it).
fn format_summary(title: &str, attribution: &str) -> String {
    if !title.trim().is_empty() {
        title.trim().to_string()
    } else if !attribution.trim().is_empty() {
        attribution.trim().to_string()
    } else {
        "NectarLink".to_string()
    }
}

/// The body: the text, a progress line, and where it came from, in italics
/// when the server shows markup and as plain text when it doesn't.
fn format_body(
    title: &str,
    body: &str,
    attribution: &str,
    progress: Option<&Progress>,
    markup: bool,
) -> String {
    let text = |s: &str| if markup { escape_notification_markup(s.trim()) } else { s.trim().to_owned() };
    let mut parts = Vec::new();
    if !body.trim().is_empty() {
        parts.push(text(body));
    }
    if let Some(prog) = progress {
        let mut prog_line = String::new();
        if !prog.status.trim().is_empty() {
            prog_line.push_str(&text(&prog.status));
        }
        if !prog.label.trim().is_empty() {
            if !prog_line.is_empty() {
                prog_line.push_str(" · ");
            }
            prog_line.push_str(&text(&prog.label));
        } else if let Some(val) = prog.value {
            let pct = (val.clamp(0.0, 1.0) * 100.0).round() as i32;
            if !prog_line.is_empty() {
                prog_line.push_str(&format!(" ({pct}%)"));
            } else {
                prog_line.push_str(&format!("{pct}%"));
            }
        }
        if !prog_line.is_empty() {
            parts.push(prog_line);
        }
    }
    // Without a title the attribution is already the summary.
    if !attribution.trim().is_empty() && !title.trim().is_empty() {
        parts.push(if markup { format!("<i>{}</i>", text(attribution)) } else { text(attribution) });
    }
    parts.join("\n")
}

fn compute_progress_percent(progress: &Progress) -> i32 {
    progress.value.map(|v| (v.clamp(0.0, 1.0) * 100.0).round() as i32).unwrap_or(0)
}

/// The `actions` argument of `Notify`: pairs of action key and label. Only
/// what the server can show: no buttons without `actions`, and a reply field
/// only with `inline-reply` (elsewhere clicking the notification opens the
/// conversation in the app, where the reply goes).
fn build_actions_list(
    actions: &[(String, String)],
    reply: Option<&(String, String)>,
    caps: Capabilities,
) -> Vec<String> {
    if !caps.actions {
        return Vec::new();
    }
    let mut items = Vec::with_capacity(2 + actions.len() * 2 + 2);
    items.push("default".to_string());
    items.push(crate::i18n::tr!("Notifications", "Open"));
    for (id, label) in actions {
        if !id.trim().is_empty() {
            items.push(id.clone());
            items.push(label.clone());
        }
    }
    if caps.inline_reply
        && let Some((_action_id, placeholder)) = reply
    {
        let label = if placeholder.trim().is_empty() {
            crate::i18n::tr!("Notifications", "Reply")
        } else {
            placeholder.trim().to_owned()
        };
        items.push("inline-reply".to_string());
        items.push(label);
    }
    items
}

fn build_hints_map(
    progress: Option<&Progress>,
    silent: bool,
    call: bool,
    image_path: Option<&str>,
) -> HashMap<&'static str, zbus::zvariant::Value<'static>> {
    let mut hints = HashMap::new();
    hints.insert("desktop-entry", zbus::zvariant::Value::from("nectarlink"));
    if call {
        hints.insert("category", zbus::zvariant::Value::from("call.incoming"));
        hints.insert("urgency", zbus::zvariant::Value::U8(2));
        hints.insert("resident", zbus::zvariant::Value::Bool(true));
    } else {
        let urgency: u8 = if silent { 0 } else { 1 };
        hints.insert("urgency", zbus::zvariant::Value::U8(urgency));
        if silent {
            hints.insert("suppress-sound", zbus::zvariant::Value::Bool(true));
        }
    }
    if let Some(img) = image_path.filter(|s| !s.is_empty()) {
        hints.insert("image-path", zbus::zvariant::Value::from(img.to_owned()));
    }
    if let Some(prog) = progress {
        let pct = compute_progress_percent(prog);
        hints.insert("value", zbus::zvariant::Value::I32(pct));
    }
    hints
}

#[allow(clippy::too_many_arguments)]
fn send_notification(
    replaces_id: u32,
    icon_path: &str,
    image_path: Option<&str>,
    summary: &str,
    body: &str,
    actions: &[(String, String)],
    reply: Option<&(String, String)>,
    progress: Option<&Progress>,
    silent: bool,
    call: bool,
) -> Option<u32> {
    if cfg!(test) {
        return None;
    }

    let actions_list = build_actions_list(actions, reply, capabilities());
    let hints_map = build_hints_map(progress, silent, call, image_path);
    let expire_timeout: i32 = if call { 0 } else { -1 };

    with_session_connection(|conn| {
        let reply_msg = conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("NectarLink", replaces_id, icon_path, summary, body, &actions_list, &hints_map, expire_timeout),
        )?;
        let msg_body = reply_msg.body();
        msg_body.deserialize::<u32>().or_else(|_| msg_body.deserialize::<(u32,)>().map(|(id,)| id))
    })
}

fn close_dbus_notification(id: u32) {
    if cfg!(test) || id == 0 {
        return;
    }
    let _ = with_session_connection(|conn| {
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "CloseNotification",
            &(id,),
        )
        .map(|_| ())
    });
}

fn monitor_loop() {
    loop {
        // Use the SAME shared D-Bus session connection as `send_notification()` so
        // unicast `ActionInvoked`, `NotificationReplied`, and `NotificationClosed`
        // signals sent to our caller unique name (`:1.x`) are received here.
        let Some(conn) = dbus_session() else {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        let Ok(rule) = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.Notifications")
            .and_then(|b| b.path("/org/freedesktop/Notifications"))
            .map(|b| b.build())
        else {
            return;
        };
        let Ok(iter) = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, None) else {
            let _ = reset_dbus_session();
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };

        for msg in iter {
            let Ok(msg) = msg else {
                let _ = reset_dbus_session();
                break;
            };
            if let Some(signal) = parse_dbus_signal(&msg) {
                let (event, callback) = TOASTER
                    .get()
                    .and_then(|mutex| {
                        let mut guard = mutex.lock().ok()?;
                        let ev = guard.handle_signal(signal);
                        let cb = guard.on_event.clone();
                        Some((ev, cb))
                    })
                    .unwrap_or((None, None));
                if let (Some(event), Some(cb)) = (event, callback) {
                    cb(event);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Deserializes a D-Bus signal from `org.freedesktop.Notifications` into a [`MonitorSignal`].
pub fn parse_dbus_signal(msg: &zbus::message::Message) -> Option<MonitorSignal> {
    let header = msg.header();
    if header.message_type() != zbus::message::Type::Signal {
        return None;
    }
    if header.interface().map(|i| i.as_str()) != Some("org.freedesktop.Notifications") {
        return None;
    }
    let member = header.member()?.as_str();
    match member {
        "ActionInvoked" => {
            let (id, action_key) = msg.body().deserialize::<(u32, String)>().ok()?;
            Some(MonitorSignal::ActionInvoked { id, action_key })
        }
        "NotificationReplied" => {
            let (id, text) = msg.body().deserialize::<(u32, String)>().ok()?;
            Some(MonitorSignal::NotificationReplied { id, text })
        }
        "NotificationClosed" => {
            let (id, reason) = msg.body().deserialize::<(u32, u32)>().ok()?;
            Some(MonitorSignal::NotificationClosed { id, reason })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_actions_and_hints_without_gvariant_text_mangling() {
        let actions = vec![("mark_read".to_string(), "Mark 'read' [1]".to_string())];
        let reply = ("direct_reply".to_string(), "Reply \"now\"".to_string());
        let kde = Capabilities { body_markup: true, actions: true, inline_reply: true };
        let list = build_actions_list(&actions, Some(&reply), kde);
        assert_eq!(
            list,
            vec!["default", "Open", "mark_read", "Mark 'read' [1]", "inline-reply", "Reply \"now\""]
        );

        let gnome = Capabilities { body_markup: true, actions: true, inline_reply: false };
        assert_eq!(
            build_actions_list(&actions, Some(&reply), gnome),
            vec!["default", "Open", "mark_read", "Mark 'read' [1]"],
            "no reply field where the server can't show one"
        );
        assert!(build_actions_list(&actions, Some(&reply), Capabilities::default()).is_empty());
        assert_eq!(
            Capabilities::from_list(&["actions".into(), "body".into(), "body-markup".into()]),
            Capabilities { body_markup: true, actions: true, inline_reply: false }
        );

        let prog = Progress { status: "Transferring".to_string(), value: Some(0.42), label: String::new() };
        let hints = build_hints_map(Some(&prog), true, false, Some("/tmp/photo.jpg"));
        assert_eq!(hints.get("desktop-entry"), Some(&zbus::zvariant::Value::from("nectarlink")));
        assert_eq!(hints.get("urgency"), Some(&zbus::zvariant::Value::U8(0)));
        assert_eq!(hints.get("suppress-sound"), Some(&zbus::zvariant::Value::Bool(true)));
        assert_eq!(hints.get("value"), Some(&zbus::zvariant::Value::I32(42)));
        assert_eq!(hints.get("image-path"), Some(&zbus::zvariant::Value::from("/tmp/photo.jpg")));

        let call_hints = build_hints_map(None, false, true, None);
        assert_eq!(call_hints.get("desktop-entry"), Some(&zbus::zvariant::Value::from("nectarlink")));
        assert_eq!(call_hints.get("category"), Some(&zbus::zvariant::Value::from("call.incoming")));
        assert_eq!(call_hints.get("urgency"), Some(&zbus::zvariant::Value::U8(2)));
        assert_eq!(call_hints.get("resident"), Some(&zbus::zvariant::Value::Bool(true)));
    }

    #[test]
    fn escapes_notification_markup_and_formats_attribution_in_italics() {
        assert_eq!(escape_notification_markup("5 < 10 & 20 > 3"), "5 &lt; 10 &amp; 20 &gt; 3");
        let formatted = format_body("Sam", "Meet at <Cafe & Bar>", "Messages & Co · Pixel 9", None, true);
        assert_eq!(formatted, "Meet at &lt;Cafe &amp; Bar&gt;\n<i>Messages &amp; Co · Pixel 9</i>");
        let plain = format_body("Sam", "Meet at <Cafe & Bar>", "Messages & Co · Pixel 9", None, false);
        assert_eq!(
            plain, "Meet at <Cafe & Bar>\nMessages & Co · Pixel 9",
            "no markup for servers without it"
        );
        let untitled = format_body("", "Battery low", "Pixel 9", None, true);
        assert_eq!(untitled, "Battery low", "the attribution is the summary then, not repeated");
        let prog = Progress { status: "Sending".into(), value: None, label: "photo.jpg".into() };
        assert_eq!(format_body("Files", "", "", Some(&prog), false), "Sending · photo.jpg");
    }

    #[test]
    fn toast_and_snapshot_debug_redact_title_and_body() {
        let toast = Toast {
            device: "phone-1".to_string(),
            key: "notif-secret".to_string(),
            title: "Secret Sender Name".to_string(),
            body: "Super confidential message body 12345".to_string(),
            attribution: "Signal · Pixel".to_string(),
            icon: None,
            image: None,
            actions: vec![("reply".to_string(), "Reply".to_string())],
            reply: None,
            silent: false,
            progress: None,
            call: false,
        };
        let debug_str = format!("{toast:?}");
        assert!(!debug_str.contains("Secret Sender Name"), "raw title leaked: {debug_str}");
        assert!(!debug_str.contains("Super confidential message body"), "raw body leaked: {debug_str}");
        assert!(debug_str.contains("<redacted>"), "{debug_str}");
        assert!(debug_str.contains("title_len"), "{debug_str}");
        assert!(debug_str.contains("body_len"), "{debug_str}");

        let snapshot = ToastSnapshot {
            title: "Private Title".to_string(),
            body: "Private Body Content".to_string(),
            attribution: "Signal · Pixel".to_string(),
            icon_path: "/tmp/icon.png".to_string(),
            image_path: None,
            actions: Vec::new(),
            reply: None,
            silent: false,
            call: false,
        };
        let snap_debug = format!("{snapshot:?}");
        assert!(!snap_debug.contains("Private Title"), "raw snapshot title leaked: {snap_debug}");
        assert!(!snap_debug.contains("Private Body Content"), "raw snapshot body leaked: {snap_debug}");
        assert!(snap_debug.contains("<redacted>"), "{snap_debug}");
    }

    #[test]
    fn parses_zbus_notification_signals() {
        let action_msg = zbus::message::Message::signal(
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
        )
        .unwrap()
        .build(&(15u32, "mark_read"))
        .unwrap();
        assert_eq!(
            parse_dbus_signal(&action_msg),
            Some(MonitorSignal::ActionInvoked { id: 15, action_key: "mark_read".to_string() })
        );

        let reply_msg = zbus::message::Message::signal(
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "NotificationReplied",
        )
        .unwrap()
        .build(&(15u32, "Sounds good's!\nLine 2 [true]"))
        .unwrap();
        assert_eq!(
            parse_dbus_signal(&reply_msg),
            Some(MonitorSignal::NotificationReplied {
                id: 15,
                text: "Sounds good's!\nLine 2 [true]".to_string(),
            })
        );

        let closed_msg = zbus::message::Message::signal(
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "NotificationClosed",
        )
        .unwrap()
        .build(&(15u32, 2u32))
        .unwrap();
        assert_eq!(
            parse_dbus_signal(&closed_msg),
            Some(MonitorSignal::NotificationClosed { id: 15, reason: 2 })
        );
    }

    #[test]
    fn toaster_tracks_lifecycle_and_routes_signals() {
        let mut toaster = Toaster::new(PathBuf::from("/tmp/icon.png"), None);
        let snap = ToastSnapshot {
            title: "Messages".to_string(),
            body: "Hi there".to_string(),
            attribution: "Messages · Pixel 9".to_string(),
            icon_path: "/tmp/icon.png".to_string(),
            image_path: None,
            actions: vec![("archive".to_string(), "Archive".to_string())],
            reply: Some(("direct_reply".to_string(), "Type a message".to_string())),
            silent: false,
            call: false,
        };

        toaster.record_shown("phone-1".to_string(), "notif-a".to_string(), 77, snap);

        assert_eq!(
            toaster.handle_signal(MonitorSignal::ActionInvoked { id: 77, action_key: "default".to_string() }),
            Some(ToastEvent::Opened { device: "phone-1".to_string(), key: "notif-a".to_string() })
        );

        assert_eq!(
            toaster.handle_signal(MonitorSignal::ActionInvoked { id: 77, action_key: "archive".to_string() }),
            Some(ToastEvent::Action {
                device: "phone-1".to_string(),
                key: "notif-a".to_string(),
                action: "archive".to_string(),
            })
        );

        assert_eq!(
            toaster
                .handle_signal(MonitorSignal::NotificationReplied { id: 77, text: "On my way".to_string() }),
            Some(ToastEvent::Reply {
                device: "phone-1".to_string(),
                key: "notif-a".to_string(),
                action: "direct_reply".to_string(),
                text: "On my way".to_string(),
            })
        );

        assert_eq!(
            toaster.handle_signal(MonitorSignal::NotificationClosed { id: 77, reason: 2 }),
            Some(ToastEvent::Dismissed { device: "phone-1".to_string(), key: "notif-a".to_string() })
        );

        assert!(toaster.active_ids.is_empty());
        assert!(toaster.id_to_target.is_empty());
    }
}
