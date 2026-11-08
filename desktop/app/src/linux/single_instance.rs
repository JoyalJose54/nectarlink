// SPDX-License-Identifier: GPL-3.0-or-later
//! Single-instance lock and wakeup on Linux using `flock(LOCK_EX | LOCK_NB)` on
//! `<data_dir>/instance.lock` and a Unix domain stream socket at `<data_dir>/instance.sock`.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

const LOCK_FILE: &str = "instance.lock";
const SOCKET_FILE: &str = "instance.sock";
const MAX_SUN_PATH_LEN: usize = 100;

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;
const LOCK_UN: i32 = 8;
const EAGAIN: i32 = 11;
const EWOULDBLOCK: i32 = 11;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// Held for the lifetime of the primary process; releasing it (on exit or crash)
/// releases the `flock` lock.
#[derive(Debug)]
pub struct PrimaryInstance {
    _lock_file: File,
    socket_path: PathBuf,
    listener: Mutex<Option<UnixListener>>,
}

#[derive(Debug)]
pub enum Instance {
    Primary(PrimaryInstance),
    Secondary,
}

fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOCK_FILE)
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn socket_path(data_dir: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    let direct = data_dir.join(SOCKET_FILE);
    if direct.as_os_str().as_bytes().len() <= MAX_SUN_PATH_LEN {
        return direct;
    }
    let canonical = fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let hash = fnv1a_64(canonical.as_os_str().as_bytes());
    let uid = unsafe { libc::geteuid() };
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.as_os_str().as_bytes().len() <= 64)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("nl-{uid}-{hash:016x}"));
    let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
    if let Ok(meta) = fs::symlink_metadata(&dir)
        && !meta.file_type().is_symlink()
        && meta.uid() == uid
    {
        return dir.join(SOCKET_FILE);
    }
    direct
}

/// Acquires the single-instance lock for `data_dir`, or reports that another
/// instance already holds it.
pub fn acquire(data_dir: &Path) -> io::Result<Instance> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(data_dir);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(lock_path(data_dir))?;

    // SAFETY: `lock_file` is an open file descriptor owned by this process.
    let rc = unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock
            || err.raw_os_error() == Some(EWOULDBLOCK)
            || err.raw_os_error() == Some(EAGAIN)
        {
            return Ok(Instance::Secondary);
        }
        return Err(err);
    }

    let sock_path = socket_path(data_dir);
    if fs::symlink_metadata(&sock_path).is_ok_and(|m| !m.file_type().is_symlink()) {
        let _ = fs::remove_file(&sock_path);
    }
    let listener = UnixListener::bind(&sock_path).ok();
    if listener.is_some() {
        let _ = fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600));
    }

    Ok(Instance::Primary(PrimaryInstance {
        _lock_file: lock_file,
        socket_path: sock_path,
        listener: Mutex::new(listener),
    }))
}

/// Signals the running primary instance for `data_dir` to wake up and drain queued requests,
/// forwarding any Wayland/X11 activation token (`XDG_ACTIVATION_TOKEN` / `DESKTOP_STARTUP_ID`).
pub fn wake(data_dir: &Path) {
    let sock_path = socket_path(data_dir);
    if let Ok(mut stream) = UnixStream::connect(sock_path) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
        let token = std::env::var("XDG_ACTIVATION_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("DESKTOP_STARTUP_ID").ok().filter(|s| !s.trim().is_empty()));
        if let Some(tok) = token {
            let clean: String = tok.chars().filter(|&c| c != '\n' && c != '\r').take(256).collect();
            let _ = writeln!(stream, "WAKE {clean}");
        } else {
            let _ = stream.write_all(b"WAKE\n");
        }
        let _ = stream.flush();
    }
}

impl PrimaryInstance {
    /// Calls `on_activate` on a background thread each time a secondary instance wakes us.
    pub fn watch(&'static self, on_activate: impl Fn() + Send + 'static) {
        let Some(listener) = self.listener.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        let _ = std::thread::Builder::new().name("single-instance".into()).spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(mut conn) => {
                        let _ = conn.set_read_timeout(Some(Duration::from_millis(250)));
                        let mut buf = [0u8; 320];
                        if let Ok(n) = conn.read(&mut buf)
                            && n > 0
                            && let Ok(line) = std::str::from_utf8(&buf[..n])
                        {
                            let trimmed = line.trim();
                            if let Some(token) = trimmed.strip_prefix("WAKE ") {
                                let token = token.trim();
                                if !token.is_empty() {
                                    crate::bridge::native::ffi::linux_set_activation_token(token);
                                }
                            }
                        }
                        on_activate();
                    }
                    Err(e) => {
                        if matches!(
                            e.kind(),
                            io::ErrorKind::Interrupted
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::ConnectionAborted
                        ) {
                            continue;
                        }
                        tracing::debug!(error = %e, "single-instance socket accept failed");
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        });
    }
}

impl Drop for PrimaryInstance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
        // SAFETY: unlocking our own file descriptor before close.
        unsafe {
            let _ = flock(self._lock_file.as_raw_fd(), LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };

    #[test]
    fn second_instance_sees_primary_and_wakes_it() {
        let dir = tempfile::tempdir().unwrap();
        let primary = match acquire(dir.path()).unwrap() {
            Instance::Primary(p) => Box::leak(Box::new(p)),
            Instance::Secondary => panic!("first must be primary"),
        };
        assert!(matches!(acquire(dir.path()).unwrap(), Instance::Secondary));

        let (tx, rx) = mpsc::channel();
        let tx = Arc::new(Mutex::new(tx));
        primary.watch(move || {
            let _ = tx.lock().unwrap().send(());
        });

        wake(dir.path());
        rx.recv_timeout(Duration::from_secs(2)).expect("primary woken");
    }
}
