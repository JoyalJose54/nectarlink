// SPDX-License-Identifier: GPL-3.0-or-later
//! "Start with Windows": an installed copy starts with Windows unless the
//! user turns it off; a build run from its folder only when turned on. Only
//! the instance with the usual data folder manages this (a test instance
//! with its own leaves it alone).
//!
//! When started at login via `--minimized`, the app defers its heavy
//! background subsystems by `startup_delay_secs` (default 20 s) so Windows
//! has time to finish loading the shell, Explorer, and system services before
//! NectarLink competes for CPU, memory and disk I/O.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use crate::win::autostart;

static MANAGED: AtomicBool = AtomicBool::new(false);

/// Entered at login via `--minimized`: sleep for `delay_secs` seconds (yielding
/// to Windows so the desktop settles) before returning, allowing callers to
/// defer heavy subsystem initialisation.
///
/// If `delay_secs` is `0` the function returns immediately.
pub fn wait_for_desktop(delay_secs: u32) {
    if delay_secs == 0 {
        return;
    }

    tracing::info!(
        delay_secs,
        "started at login; waiting for desktop to settle before initialising"
    );

    std::thread::sleep(Duration::from_secs(u64::from(delay_secs)));

    tracing::info!("desktop settle delay complete; continuing startup");
}

/// Whether the app starts with Windows, given what the user chose (if
/// anything).
pub fn effective(choice: Option<bool>) -> bool {
    choice.unwrap_or_else(|| std::env::current_exe().is_ok_and(|exe| autostart::is_installed(&exe)))
}

/// Brings the `Run` entry in line with the setting, from now on.
pub fn start(choice: Option<bool>) {
    MANAGED.store(true, Ordering::Relaxed);
    apply(effective(choice));
}

/// Starts this copy with Windows, or stops doing so.
pub fn apply(on: bool) {
    if !MANAGED.load(Ordering::Relaxed) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    if let Err(e) = autostart::set(on, &exe) {
        tracing::warn!(error = %e, "can't change starting with Windows");
    }
}

/// Removes the entry (when uninstalling).
pub fn remove() {
    if let Ok(exe) = std::env::current_exe()
        && let Err(e) = autostart::set(false, &exe)
    {
        tracing::warn!(error = %e, "can't stop starting with Windows");
    }
}
