// SPDX-License-Identifier: GPL-3.0-or-later
use std::fs;
use std::io::Result;
use std::path::Path;

use crate::mark;

use super::image::{Bitmap, encode_png};

pub fn write_png(path: &Path, size: u32) -> Result<()> {
    let rgba = mark::render(size as usize);
    let mut bgra = Vec::with_capacity(rgba.len());
    for px in rgba.as_chunks::<4>().0 {
        bgra.push(px[2]);
        bgra.push(px[1]);
        bgra.push(px[0]);
        bgra.push(px[3]);
    }
    let bitmap = Bitmap { width: size, height: size, bgra };
    let png = encode_png(&bitmap)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
        if let Some(parent) = path.parent() {
            let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
            if let Ok(meta) = fs::symlink_metadata(parent) {
                let euid = unsafe { libc::geteuid() };
                if meta.file_type().is_symlink() || meta.uid() != euid {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "refusing to write icon into untrusted directory",
                    ));
                }
            }
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o644)
            .open(path)?;
        file.write_all(&png)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, png)
    }
}
