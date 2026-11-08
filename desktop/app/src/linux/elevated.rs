// SPDX-License-Identifier: GPL-3.0-or-later
//! Running a process with root privileges on Linux via `pkexec` (Polkit).

use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::Duration,
};

/// Why starting or running the elevated process failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The user dismissed or declined the Polkit prompt (`pkexec` exit status 126 or 127).
    Declined,
    Other(String),
}

impl Error {
    #[allow(non_snake_case)]
    pub fn Failed(msg: String) -> Self {
        Self::Other(msg)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined => write!(f, "authentication declined"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// A running elevated child process.
#[derive(Debug)]
pub struct Process(Mutex<Child>);

unsafe extern "C" {
    fn geteuid() -> u32;
}

/// Returns `true` if the current process is already running as `root` (`euid == 0`).
pub fn is_elevated() -> bool {
    // SAFETY: `geteuid` is always safe and has no preconditions.
    unsafe { geteuid() == 0 }
}

fn has_pkexec() -> bool {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let extra = [Path::new("/usr/bin"), Path::new("/bin")];
    std::env::split_paths(&path_var).chain(extra.into_iter().map(Path::to_path_buf)).any(|dir| {
        let candidate = dir.join("pkexec");
        fs::metadata(&candidate).is_ok_and(|m| {
            if !m.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                m.mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        })
    })
}

pub fn classify_exit_code(code: Option<i32>, ran_via_pkexec: bool) -> Result<i32, Error> {
    match code {
        Some(126 | 127) if ran_via_pkexec => Err(Error::Declined),
        Some(c) => Ok(c),
        None => Ok(1),
    }
}

fn split_params(params: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in params.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Starts `program` with `params` under `pkexec` (or directly when already `root`),
/// without waiting for it to finish.
pub fn start(program: &Path, params: &str) -> Result<Process, Error> {
    let elevated = is_elevated();
    if !elevated && !has_pkexec() {
        return Err(Error::Failed("pkexec not found".into()));
    }

    let parsed = split_params(params);
    let mut cmd = if elevated {
        let mut c = Command::new(program);
        c.args(&parsed);
        c
    } else {
        let mut c = Command::new("pkexec");
        c.arg(program).args(&parsed);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| Error::Failed(e.to_string()))?;

    if !elevated {
        for _ in 0..10 {
            match child.try_wait() {
                Ok(Some(status)) => {
                    classify_exit_code(status.code(), true)?;
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(Error::Failed(e.to_string())),
            }
        }
    }

    Ok(Process(Mutex::new(child)))
}

/// Runs `program` with `args` under `pkexec` (or directly when already `root`)
/// and waits for its exit code, returning `Err(Error::Declined)` when `pkexec`
/// exits with `126` or `127`.
pub fn run(program: &Path, args: &[&str]) -> Result<i32, Error> {
    let elevated = is_elevated();
    if !elevated && !has_pkexec() {
        return Err(Error::Failed("pkexec not found".into()));
    }

    let mut cmd = if elevated {
        let mut c = Command::new(program);
        c.args(args);
        c
    } else {
        let mut c = Command::new("pkexec");
        c.arg(program).args(args);
        c
    };
    let status = cmd.status().map_err(|e| Error::Failed(e.to_string()))?;
    classify_exit_code(status.code(), !elevated)
}

/// Runs `program` with `args` under `pkexec` and waits for its exit code.
pub fn run_elevated(program: &Path, args: &[&str]) -> Result<i32, String> {
    run(program, args).map_err(|e| e.to_string())
}

impl Process {
    /// `None` while the process is still running; `Some(code)` once it has exited.
    pub fn exit_code(&self) -> Option<u32> {
        let mut child = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(1) as u32),
            Ok(None) => None,
            Err(_) => Some(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_and_unquoted_params() {
        let args = split_params("--install-to \"/opt/NectarLink App\" --quiet");
        assert_eq!(args, vec!["--install-to", "/opt/NectarLink App", "--quiet"]);
    }

    #[test]
    fn classifies_pkexec_dismissal_and_denial_codes_as_declined() {
        assert_eq!(classify_exit_code(Some(126), true), Err(Error::Declined));
        assert_eq!(classify_exit_code(Some(127), true), Err(Error::Declined));
        assert_eq!(classify_exit_code(Some(0), true), Ok(0));
        assert_eq!(classify_exit_code(Some(1), true), Ok(1));
        assert_eq!(classify_exit_code(Some(127), false), Ok(127));
    }
}
