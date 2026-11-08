// SPDX-License-Identifier: GPL-3.0-or-later
// Linux file-manager "Send to" integration and `.lnk`-compatible shortcut metadata.
//
// `crate::send_to` reconciles per-device `<Name>.lnk` files in `send_to_dir()`.
// On Linux we store those canonical `.lnk` metadata files inside
// `~/.local/share/nectarlink/send-to/` and mirror them into the file managers'
// right-click menus:
//   1. GNOME Files (Nautilus): `~/.local/share/nautilus/scripts/Send to <Name> (NectarLink)`
//   2. KDE Dolphin (KIO ServiceMenus): `~/.local/share/kio/servicemenus/nectarlink-send-<id>.desktop`
//   3. MATE Caja (with caja-actions; File Manager Actions):
//      `~/.local/share/file-manager/actions/nectarlink-send-<id>.desktop`
//   4. Cinnamon Nemo: `~/.local/share/nemo/actions/nectarlink-send-<id>.nemo_action`
//   5. XFCE Thunar: our `<action>` entries in `~/.config/Thunar/uca.xml`, next to
//      the user's own custom actions (only when Thunar's folder exists).

#![allow(dead_code)]

use std::io::Result;
use std::path::{Path, PathBuf};

/// What a shortcut starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub target: PathBuf,
    pub arguments: String,
    /// The tooltip.
    pub description: String,
}

fn xdg_data_home() -> Option<PathBuf> {
    dirs::data_local_dir().filter(|p| p.is_absolute())
}

fn sanitize_single_line(s: &str) -> String {
    s.chars().filter(|&c| c != '\n' && c != '\r' && !c.is_control()).collect()
}

fn escape_posix_sh_single_quoted(target: &str) -> String {
    sanitize_single_line(target).replace('\'', "'\\''")
}

fn sanitize_exec_args(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            c != '\n'
                && c != '\r'
                && c != '$'
                && c != '`'
                && c != ';'
                && c != '&'
                && c != '|'
                && c != '<'
                && c != '>'
                && !c.is_control()
        })
        .collect()
}

fn write_file_nofollow(path: &Path, content: &str, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(mode)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::write(path, content)
    }
}

/// Write a shortcut entry to `path`, and if `path` lives in the active `send_to_dir()`,
/// synchronize GNOME Files (Nautilus), KDE Dolphin, and Nemo/Thunar context-menu entries.
pub fn write(path: &Path, shortcut: &Shortcut) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw_target = sanitize_single_line(&shortcut.target.to_string_lossy());
    let exec_target = super::autostart::escape_desktop_exec_arg(&shortcut.target.to_string_lossy());
    let args = sanitize_single_line(&shortcut.arguments);
    let desc = sanitize_single_line(&shortcut.description);

    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=NectarLink\n\
         Comment={desc}\n\
         Exec=\"{exec_target}\" {args}\n\
         Icon=nectarlink\n\
         Terminal=false\n\
         X-NectarLink-Target={raw_target}\n\
         X-NectarLink-Args={args}\n\
         X-NectarLink-Comment={desc}\n",
    );
    write_file_nofollow(path, &content, 0o644)?;

    if let (Some(parent), Some(real_send_to)) = (path.parent(), send_to_dir())
        && parent == real_send_to
    {
        let _ = sync_file_manager_menus(parent);
    }

    Ok(())
}

/// The target path a shortcut points at, or `None` if it can't be read.
pub fn target(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("X-NectarLink-Target=") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    None
}

/// The arguments a shortcut passes, or `None` if it can't be read.
pub fn arguments(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("X-NectarLink-Args=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Read a full [`Shortcut`] metadata record from `path`.
fn read_shortcut(path: &Path) -> Option<Shortcut> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut target = None;
    let mut args = None;
    let mut comment = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("X-NectarLink-Target=") {
            target = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("X-NectarLink-Args=") {
            args = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("X-NectarLink-Comment=") {
            comment = rest.to_string();
        }
    }
    Some(Shortcut { target: target?, arguments: args?, description: comment })
}

/// Resolve the user's Startup / Autostart folder (`~/.config/autostart`).
pub fn startup_dir() -> Option<PathBuf> {
    let base = dirs::config_dir().filter(|p| p.is_absolute())?;
    Some(base.join("autostart"))
}

/// Resolve the user's canonical SendTo directory (`~/.local/share/nectarlink/send-to`).
/// Ensures the directory exists so `send_to::reconcile` can enumerate it cleanly.
pub fn send_to_dir() -> Option<PathBuf> {
    let dir = xdg_data_home()?.join("nectarlink/send-to");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// Resolve the user's Recent items directory (`~/.local/share/nectarlink/recent`).
pub fn recent_dir() -> Option<PathBuf> {
    let dir = xdg_data_home()?.join("nectarlink/recent");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// Synchronize all NectarLink `.lnk` entries in `send_to_dir` into the user's
/// file-manager context menus (see the top of this file).
pub fn sync_file_manager_menus(send_to_dir: &Path) -> Result<()> {
    let Some(data_home) = xdg_data_home() else {
        return Ok(());
    };
    let entries = collect_shortcuts(send_to_dir)?;
    write_file_manager_integrations(&data_home, &entries)?;
    if let Some(config) = xdg_config_home() {
        write_thunar_actions(&config, &entries)?;
    }
    Ok(())
}

/// Remove all NectarLink file-manager context menu entries.
pub fn remove_all_file_manager_menus() -> Result<()> {
    let Some(data_home) = xdg_data_home() else {
        return Ok(());
    };
    write_file_manager_integrations(&data_home, &[])?;
    if let Some(config) = xdg_config_home() {
        write_thunar_actions(&config, &[])?;
    }
    Ok(())
}

fn xdg_config_home() -> Option<PathBuf> {
    dirs::config_dir().filter(|p| p.is_absolute())
}

fn collect_shortcuts(send_to_dir: &Path) -> Result<Vec<(PathBuf, Shortcut)>> {
    let mut out = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(send_to_dir) else {
        return Ok(out);
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
            continue;
        }
        if let Some(sc) = read_shortcut(&path)
            && sc.arguments.starts_with("--send-to ")
        {
            out.push((path, sc));
        }
    }
    Ok(out)
}

fn extract_device_id(args: &str) -> String {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    for pair in tokens.windows(2) {
        if pair[0] == "--send-to" {
            let sanitized: String = pair[1]
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect();
            if !sanitized.is_empty() {
                return sanitized;
            }
        }
    }
    "default".to_string()
}

fn format_menu_label(path: &Path) -> String {
    let raw_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Phone");
    let cleaned: String = raw_stem
        .chars()
        .filter(|&c| c != '/' && c != '\\' && c != '\n' && c != '\r' && !c.is_control())
        .collect();
    let stem = cleaned.replace("..", "").trim().to_string();
    let stem = if stem.is_empty() { "Phone" } else { stem.as_str() };
    let with_prefix = if stem.starts_with("Send to ") { stem.to_string() } else { format!("Send to {stem}") };
    if with_prefix.ends_with("(NectarLink)") || with_prefix.contains("(NectarLink ") {
        with_prefix
    } else {
        format!("{with_prefix} (NectarLink)")
    }
}

/// Write the Nautilus, KDE Dolphin, Caja and Nemo entries under `root_data_dir`
/// (e.g. `~/.local/share`) and prune stale NectarLink entries.
pub fn write_file_manager_integrations(
    root_data_dir: &Path,
    shortcuts: &[(PathBuf, Shortcut)],
) -> Result<()> {
    let nautilus_dir = root_data_dir.join("nautilus/scripts");
    let kio_dir = root_data_dir.join("kio/servicemenus");
    let actions_dir = root_data_dir.join("file-manager/actions");
    let nemo_dir = root_data_dir.join("nemo/actions");

    if !shortcuts.is_empty() {
        std::fs::create_dir_all(&nautilus_dir)?;
        std::fs::create_dir_all(&kio_dir)?;
        std::fs::create_dir_all(&actions_dir)?;
        std::fs::create_dir_all(&nemo_dir)?;
    }

    let mut desired_nautilus = std::collections::HashSet::new();
    let mut desired_desktop = std::collections::HashSet::new();
    let mut desired_nemo = std::collections::HashSet::new();

    for (lnk_path, sc) in shortcuts {
        let label = format_menu_label(lnk_path);
        let dev_id = extract_device_id(&sc.arguments);
        let desktop_filename = format!("nectarlink-send-{dev_id}.desktop");

        desired_nautilus.insert(label.clone());
        desired_desktop.insert(desktop_filename.clone());

        let shell_target = escape_posix_sh_single_quoted(&sc.target.to_string_lossy());
        let exec_target = super::autostart::escape_desktop_exec_arg(&sc.target.to_string_lossy());
        let args = sanitize_exec_args(&sc.arguments);
        let comment = sanitize_single_line(&sc.description);

        // 1. GNOME Files (Nautilus) executable script
        let nautilus_path = nautilus_dir.join(&label);
        let script = format!(
            "#!/usr/bin/env sh\n\
             # Generated by NectarLink: {comment}\n\
             if [ -n \"${{NAUTILUS_SCRIPT_SELECTED_FILE_PATHS:-}}\" ]; then\n\
                 IFS='\n'\n\
                 set -f\n\
                 exec '{shell_target}' {args} $NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\n\
             else\n\
                 exec '{shell_target}' {args} \"$@\"\n\
             fi\n",
        );
        write_file_nofollow(&nautilus_path, &script, 0o755)?;

        // 2. KDE Dolphin (KIO ServiceMenu): must be 0o755 so Plasma 6 KFileItemActions executes it
        let kio_path = kio_dir.join(&desktop_filename);
        let kio_content = format!(
            "[Desktop Entry]\n\
             Type=Service\n\
             MimeType=all/all;\n\
             Actions=NectarLinkSend_{dev_id};\n\
             X-KDE-Priority=TopLevel\n\
             \n\
             [Desktop Action NectarLinkSend_{dev_id}]\n\
             Name={label}\n\
             Icon=nectarlink\n\
             Exec=\"{exec_target}\" {args} %F\n"
        );
        write_file_nofollow(&kio_path, &kio_content, 0o755)?;

        // 3. Caja (DES-EMA File Manager Action)
        let action_path = actions_dir.join(&desktop_filename);
        let action_content = format!(
            "[Desktop Entry]\n\
             Type=Action\n\
             Name={label}\n\
             Tooltip={comment}\n\
             Icon=nectarlink\n\
             Profiles=profile-zero;\n\
             \n\
             [X-Action-Profile profile-zero]\n\
             MimeTypes=all/all;\n\
             Exec=\"{exec_target}\" {args} %F\n\
             Name=Default profile\n",
        );
        write_file_nofollow(&action_path, &action_content, 0o644)?;

        // 4. Nemo. Its Name takes %-codes, so the phone's name loses any '%'.
        let nemo_filename = format!("nectarlink-send-{dev_id}.nemo_action");
        desired_nemo.insert(nemo_filename.clone());
        let nemo_label = label.replace('%', "");
        let nemo_content = format!(
            "[Nemo Action]\n\
             Name={nemo_label}\n\
             Comment={comment}\n\
             Exec=\"{exec_target}\" {args} %F\n\
             Icon-Name=nectarlink\n\
             Selection=notnone\n\
             Extensions=any;\n\
             Quote=double\n"
        );
        write_file_nofollow(&nemo_dir.join(&nemo_filename), &nemo_content, 0o644)?;
    }

    // Prune stale Nautilus scripts generated by NectarLink
    if let Ok(rd) = std::fs::read_dir(&nautilus_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                let is_nectarlink = name.ends_with("(NectarLink)") || name.contains("(NectarLink ");
                if is_nectarlink && !desired_nautilus.contains(name) {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }

    // Prune stale Nemo actions.
    if let Ok(rd) = std::fs::read_dir(&nemo_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if let Some(name) = p.file_name().and_then(|s| s.to_str())
                && name.starts_with("nectarlink-")
                && name.ends_with(".nemo_action")
                && !desired_nemo.contains(name)
            {
                let _ = std::fs::remove_file(&p);
            }
        }
    }

    // Prune stale KDE ServiceMenus and File Manager Actions prefixed with `nectarlink-`
    for dir in [&kio_dir, &actions_dir] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if let Some(name) = p.file_name().and_then(|s| s.to_str())
                    && name.starts_with("nectarlink-")
                    && name.ends_with(".desktop")
                    && !desired_desktop.contains(name)
                {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }

    Ok(())
}

/// Thunar keeps every custom action in one file, `<config>/Thunar/uca.xml`,
/// with the user's own. Replaces NectarLink's entries there (`unique-id`
/// `nectarlink-send-…`) with `shortcuts`, keeping everything else as it was.
/// Nothing is created for a PC without Thunar, and a file that doesn't look
/// like Thunar's is left alone.
pub fn write_thunar_actions(config_dir: &Path, shortcuts: &[(PathBuf, Shortcut)]) -> Result<()> {
    let thunar_dir = config_dir.join("Thunar");
    if !thunar_dir.is_dir() {
        return Ok(());
    }
    let path = thunar_dir.join("uca.xml");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if existing.is_none() && shortcuts.is_empty() {
        return Ok(());
    }
    let base = existing
        .clone()
        .unwrap_or_else(|| "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n</actions>\n".to_string());
    let Some(updated) = thunar_uca_with(&base, shortcuts) else {
        return Ok(());
    };
    if existing.as_deref() == Some(updated.as_str()) {
        return Ok(());
    }
    // Written beside it and renamed over it, so Thunar never reads half a file.
    let temp = thunar_dir.join(".uca.xml.nectarlink");
    write_file_nofollow(&temp, &updated, 0o644)?;
    std::fs::rename(&temp, &path)
}

/// `uca` without NectarLink's actions, plus one for each of `shortcuts`;
/// `None` when it has no closing `</actions>` to put them before.
fn thunar_uca_with(uca: &str, shortcuts: &[(PathBuf, Shortcut)]) -> Option<String> {
    let mut kept = String::with_capacity(uca.len());
    let mut rest = uca;
    while let Some(start) = rest.find("<action>") {
        let end = start + rest[start..].find("</action>")? + "</action>".len();
        if rest[start..end].contains("<unique-id>nectarlink-send-") {
            // Dropped with the indentation before it and the line break after it.
            kept.push_str(rest[..start].trim_end_matches([' ', '\t']));
            rest = rest[end..].strip_prefix('\n').unwrap_or(&rest[end..]);
        } else {
            kept.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }
    kept.push_str(rest);

    let close = kept.rfind("</actions>")?;
    let mut ours = String::new();
    for (lnk_path, sc) in shortcuts {
        let label = xml_escape(&format_menu_label(lnk_path));
        let dev_id = extract_device_id(&sc.arguments);
        let command = xml_escape(&format!(
            "'{}' {} %F",
            escape_posix_sh_single_quoted(&sc.target.to_string_lossy()),
            sanitize_exec_args(&sc.arguments)
        ));
        let comment = xml_escape(&sanitize_single_line(&sc.description));
        ours.push_str(&format!(
            "<action>\n\
             \t<icon>nectarlink</icon>\n\
             \t<name>{label}</name>\n\
             \t<submenu></submenu>\n\
             \t<unique-id>nectarlink-send-{dev_id}</unique-id>\n\
             \t<command>{command}</command>\n\
             \t<description>{comment}</description>\n\
             \t<range></range>\n\
             \t<patterns>*</patterns>\n\
             \t<directories/>\n\
             \t<audio-files/>\n\
             \t<image-files/>\n\
             \t<other-files/>\n\
             \t<text-files/>\n\
             \t<video-files/>\n\
             </action>\n"
        ));
    }
    let (head, tail) = kept.split_at(close);
    Some(format!("{head}{ours}{tail}"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Pixel 9 é.lnk");
        let shortcut = Shortcut {
            target: std::env::current_exe().unwrap(),
            arguments: r#"--send-to abc --data-dir "/tmp/some dir""#.into(),
            description: "Send to Pixel".into(),
        };
        write(&path, &shortcut).unwrap();
        assert_eq!(arguments(&path).as_deref(), Some(shortcut.arguments.as_str()));
        // Replacing works too.
        write(&path, &Shortcut { arguments: "--send-to def".into(), ..shortcut }).unwrap();
        assert_eq!(arguments(&path).as_deref(), Some("--send-to def"));
        assert_eq!(arguments(&dir.path().join("missing.lnk")), None);
    }

    #[test]
    fn file_manager_integrations_create_and_prune_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let send_to = tmp.path().join("nectarlink/send-to");
        std::fs::create_dir_all(&send_to).unwrap();

        let lnk1 = send_to.join("Pixel 9.lnk");
        let sc1 = Shortcut {
            target: PathBuf::from("/opt/user's bin/nectarlink-desktop"),
            arguments: "--send-to pixel9".to_string(),
            description: "Send to Pixel 9".to_string(),
        };
        let lnk2 = send_to.join("Galaxy S25 (NectarLink).lnk");
        let sc2 = Shortcut {
            target: PathBuf::from("/usr/bin/nectarlink-desktop"),
            arguments: "--send-to s25".to_string(),
            description: "Send to Galaxy S25".to_string(),
        };

        write_file_manager_integrations(tmp.path(), &[(lnk1.clone(), sc1.clone()), (lnk2.clone(), sc2)])
            .expect("write integrations");

        let nautilus_p1 = tmp.path().join("nautilus/scripts/Send to Pixel 9 (NectarLink)");
        let nautilus_p2 = tmp.path().join("nautilus/scripts/Send to Galaxy S25 (NectarLink)");
        let kio_p1 = tmp.path().join("kio/servicemenus/nectarlink-send-pixel9.desktop");
        let kio_p2 = tmp.path().join("kio/servicemenus/nectarlink-send-s25.desktop");
        let act_p1 = tmp.path().join("file-manager/actions/nectarlink-send-pixel9.desktop");

        assert!(nautilus_p1.exists());
        assert!(nautilus_p2.exists());
        assert!(kio_p1.exists());
        assert!(kio_p2.exists());
        assert!(act_p1.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let nautilus_mode = std::fs::metadata(&nautilus_p1).unwrap().permissions().mode();
            assert_ne!(nautilus_mode & 0o111, 0, "Nautilus script must be executable");
            let kio_mode = std::fs::metadata(&kio_p1).unwrap().permissions().mode();
            assert_eq!(kio_mode & 0o777, 0o755, "KDE ServiceMenu .desktop must be 0o755");
        }

        let nautilus_text = std::fs::read_to_string(&nautilus_p1).unwrap();
        assert!(nautilus_text.contains("exec '/opt/user'\\''s bin/nectarlink-desktop' --send-to pixel9"));

        let kio_text = std::fs::read_to_string(&kio_p1).unwrap();
        assert!(kio_text.contains("Exec=\"/opt/user's bin/nectarlink-desktop\" --send-to pixel9 %F"));

        // Now prune Galaxy S25 and verify its files are removed while Pixel 9 remains.
        write_file_manager_integrations(tmp.path(), &[(lnk1, sc1)]).expect("prune integrations");
        assert!(nautilus_p1.exists());
        assert!(!nautilus_p2.exists());
        assert!(kio_p1.exists());
        assert!(!kio_p2.exists());

        // Removing all entries prunes everything.
        write_file_manager_integrations(tmp.path(), &[]).expect("remove all integrations");
        assert!(!nautilus_p1.exists());
        assert!(!kio_p1.exists());
        assert!(!act_p1.exists());
    }

    #[test]
    fn nemo_actions_are_written_and_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let sc = Shortcut {
            target: PathBuf::from("/usr/bin/nectarlink-desktop"),
            arguments: "--send-to pixel9".to_string(),
            description: "Send to Pixel 9".to_string(),
        };
        let lnk = tmp.path().join("100% Pixel.lnk");
        write_file_manager_integrations(tmp.path(), &[(lnk, sc)]).unwrap();
        let nemo = tmp.path().join("nemo/actions/nectarlink-send-pixel9.nemo_action");
        let text = std::fs::read_to_string(&nemo).unwrap();
        assert!(text.starts_with("[Nemo Action]\n"));
        assert!(text.contains("\nName=Send to 100 Pixel (NectarLink)\n"), "{text}");
        assert!(text.contains("\nExec=\"/usr/bin/nectarlink-desktop\" --send-to pixel9 %F\n"));
        assert!(text.contains("\nExtensions=any;\n"));
        write_file_manager_integrations(tmp.path(), &[]).unwrap();
        assert!(!nemo.exists());
    }

    #[test]
    fn thunar_actions_keep_the_users_own() {
        let tmp = tempfile::tempdir().unwrap();
        let sc = Shortcut {
            target: PathBuf::from("/opt/user's bin/nectarlink-desktop"),
            arguments: "--send-to pixel9".to_string(),
            description: "Send to Pixel & co".to_string(),
        };
        let lnk = tmp.path().join("Pixel <9>.lnk");

        // No Thunar on this PC: nothing is created.
        write_thunar_actions(tmp.path(), &[(lnk.clone(), sc.clone())]).unwrap();
        assert!(!tmp.path().join("Thunar").exists());

        let dir = tmp.path().join("Thunar");
        std::fs::create_dir_all(&dir).unwrap();
        let uca = dir.join("uca.xml");
        let users = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n<action>\n\t<icon>utilities-terminal</icon>\n\t<name>Open Terminal Here</name>\n\t<unique-id>1-1</unique-id>\n\t<command>exo-open --working-directory %f --launch TerminalEmulator</command>\n\t<directories/>\n</action>\n</actions>\n";
        std::fs::write(&uca, users).unwrap();

        write_thunar_actions(tmp.path(), &[(lnk.clone(), sc.clone())]).unwrap();
        let text = std::fs::read_to_string(&uca).unwrap();
        assert!(text.contains("<name>Open Terminal Here</name>"));
        assert!(text.contains("<name>Send to Pixel &lt;9&gt; (NectarLink)</name>"), "{text}");
        assert!(text.contains("<unique-id>nectarlink-send-pixel9</unique-id>"));
        assert!(text.contains(
            "<command>&apos;/opt/user&apos;\\&apos;&apos;s bin/nectarlink-desktop&apos; --send-to pixel9 %F</command>"
        ));
        assert!(text.contains("<description>Send to Pixel &amp; co</description>"));
        assert!(text.trim_end().ends_with("</actions>"));

        // Writing again changes nothing; removing ours gives back the user's file.
        write_thunar_actions(tmp.path(), &[(lnk, sc)]).unwrap();
        assert_eq!(std::fs::read_to_string(&uca).unwrap(), text);
        write_thunar_actions(tmp.path(), &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&uca).unwrap(), users);

        // A file that isn't Thunar's is left alone.
        std::fs::write(&uca, "not xml").unwrap();
        write_thunar_actions(tmp.path(), &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&uca).unwrap(), "not xml");
    }
}
