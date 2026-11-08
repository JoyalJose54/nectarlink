// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS notifications via AppleScript.

use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

#[allow(dead_code)]
pub const AUMID: &str = "com.thtbee.nectarlink";
pub const CALL_ANSWER: &str = "answer";
pub const CALL_DECLINE: &str = "decline";
pub const CALL_END: &str = "end";

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastEvent {
    Opened { device: String, key: String },
    Action { device: String, key: String, action: String },
    Reply { device: String, key: String, action: String, text: String },
    Dismissed { device: String, key: String },
}

#[allow(dead_code)]
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

impl std::fmt::Debug for Toast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Toast").field("device", &self.device).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub status: String,
    pub value: Option<f64>,
    pub label: String,
}

type OnEvent = Box<dyn Fn(ToastEvent) + Send + Sync>;
static ON_EVENT: OnceLock<Mutex<Option<OnEvent>>> = OnceLock::new();

pub fn enabled() -> bool {
    true
}

pub fn set_process_id() {}

pub fn unregister() {}

pub fn start(_icon: PathBuf, on_event: impl Fn(ToastEvent) + Send + Sync + 'static) {
    let _ = ON_EVENT.set(Mutex::new(Some(Box::new(on_event))));
}

pub fn show(toast: Toast) {
    let script = if toast.attribution.is_empty() {
        format!("display notification {:?} with title {:?}", toast.body, toast.title)
    } else {
        format!(
            "display notification {:?} with title {:?} subtitle {:?}",
            toast.body, toast.title, toast.attribution
        )
    };
    let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
}

pub fn update_progress(_device: &str, _key: &str, _progress: Progress) {}

pub fn remove(_device: &str, _key: &str) {}

pub fn remove_device(_device: &str) {}
