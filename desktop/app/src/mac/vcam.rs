// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS virtual camera stubs.

pub const REGISTER_ARG: &str = "--register-vcam";
pub const UNREGISTER_ARG: &str = "--unregister-vcam";

pub fn is_registered() -> bool {
    false
}

pub fn register_hklm() -> Result<(), String> {
    Ok(())
}

pub fn unregister_hklm() -> Result<(), String> {
    Ok(())
}

pub fn run_elevated(_arg: &'static str, on_done: impl FnOnce(bool) + Send + 'static) {
    on_done(false);
}

#[derive(Debug, Default)]
pub struct VirtualCameraHandle;

impl VirtualCameraHandle {
    pub fn start() -> Result<Self, String> {
        Err("virtual camera is not supported on macOS".into())
    }
}
