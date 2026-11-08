// SPDX-License-Identifier: GPL-3.0-or-later
//! Starting a program with administrator rights (the UAC prompt) and
//! following it (the installer and uninstaller, `crate::setup`).

use std::path::Path;

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0},
        System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        UI::{
            Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

pub enum Error {
    Declined,
    Other(String),
}

pub struct Process(HANDLE);

// SAFETY: a process handle may be waited on from any thread.
unsafe impl Send for Process {}

impl Process {
    /// The exit code once it has exited.
    pub fn exit_code(&self) -> Option<u32> {
        if self.0.is_invalid() {
            return Some(0);
        }
        // SAFETY: a valid process handle, owned by this value.
        unsafe {
            if WaitForSingleObject(self.0, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 1u32;
            GetExitCodeProcess(self.0, &mut code).ok()?;
            Some(code)
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: closed once, here.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// Starts `program` with administrator rights (the UAC prompt).
pub fn start(program: &Path, params: &str) -> Result<Process, Error> {
    let file = HSTRING::from(program.as_os_str());
    let parameters = HSTRING::from(params);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: the structure and its strings outlive the call.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(Process(info.hProcess)),
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => Err(Error::Declined),
        Err(e) => Err(Error::Other(e.message())),
    }
}
