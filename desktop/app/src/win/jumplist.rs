// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows Taskbar Jump List: quick user tasks when right-clicking the
//! NectarLink icon on the taskbar or Start menu (`ICustomDestinationList`).

use std::path::Path;

use windows::{
    Win32::{
        Storage::EnhancedStorage::PKEY_Title,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, StructuredStorage::PROPVARIANT},
        UI::Shell::{
            Common::{IObjectArray, IObjectCollection},
            DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
            PropertiesSystem::IPropertyStore,
            ShellLink,
        },
    },
    core::{HSTRING, Interface},
};

use super::with_com;

/// The command-line flag Jump List tasks pass back to NectarLink.
pub const ARG: &str = "--task";

/// The 5 user tasks shown in the Windows Taskbar Jump List: `(action, title, description)`.
pub const TASKS: &[(&str, &str, &str)] = &[
    ("send-files", "Send files", "Choose files to send to your phone"),
    ("send-clipboard", "Send clipboard", "Send what's copied on this PC to your phone"),
    ("find-phone", "Find my phone", "Make your phone ring, even on silent"),
    ("take-photo", "Take photo with phone", "Take a photo with your phone's camera and paste it here"),
    ("open-timeline", "Open Timeline", "Open your cross-device Timeline"),
];

/// Builds the command-line arguments for a Jump List task, preserving
/// `--data-dir` when running with a custom data directory.
pub fn build_task_arguments(action: &str, custom_data_dir: Option<&Path>) -> String {
    if let Some(dir) = custom_data_dir {
        format!("--data-dir \"{}\" {ARG} {action}", dir.display())
    } else {
        format!("{ARG} {action}")
    }
}

/// Registers the 5 user tasks in the Windows Taskbar Jump List for [`crate::win::toast::AUMID`].
pub fn register(custom_data_dir: Option<&Path>) {
    let Ok(exe) = std::env::current_exe() else { return };
    if let Err(e) = try_register(&exe, custom_data_dir) {
        tracing::debug!(error = %e, "couldn't update the taskbar Jump List");
    }
}

fn try_register(exe: &Path, custom_data_dir: Option<&Path>) -> windows::core::Result<()> {
    with_com(|| {
        // SAFETY: Standard Win32 Shell COM calls (`ICustomDestinationList`, `IObjectCollection`,
        // `IShellLinkW`, `IPropertyStore`) on an apartment-initialized thread.
        unsafe {
            let dest_list: ICustomDestinationList =
                CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
            dest_list.SetAppID(&HSTRING::from(crate::win::toast::AUMID))?;
            let mut min_slots = 0u32;
            let _removed: IObjectArray = dest_list.BeginList(&mut min_slots)?;

            let collection: IObjectCollection =
                CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
            let exe_h = HSTRING::from(exe.as_os_str());

            for &(action, title, description) in TASKS {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&exe_h)?;
                let args = build_task_arguments(action, custom_data_dir);
                link.SetArguments(&HSTRING::from(args.as_str()))?;
                link.SetDescription(&HSTRING::from(description))?;
                link.SetIconLocation(&exe_h, 0)?;

                let store: IPropertyStore = link.cast()?;
                store.SetValue(&PKEY_Title, &PROPVARIANT::from(title))?;
                store.Commit()?;

                collection.AddObject(&link)?;
            }

            let tasks_array: IObjectArray = collection.cast()?;
            dest_list.AddUserTasks(&tasks_array)?;
            dest_list.CommitList()
        }
    })
}

/// Removes the app's Jump List when uninstalling.
pub fn unregister() {
    with_com(|| {
        // SAFETY: Standard Win32 Shell COM call on `ICustomDestinationList`.
        unsafe {
            if let Ok(dest_list) =
                CoCreateInstance::<_, ICustomDestinationList>(&DestinationList, None, CLSCTX_INPROC_SERVER)
            {
                let _ = dest_list.DeleteList(&HSTRING::from(crate::win::toast::AUMID));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_arguments_preserve_custom_data_dir() {
        assert_eq!(build_task_arguments("send-files", None), "--task send-files");
        let custom = Path::new(r"C:\Temp\nectarlink-test");
        assert_eq!(
            build_task_arguments("find-phone", Some(custom)),
            r#"--data-dir "C:\Temp\nectarlink-test" --task find-phone"#
        );
        assert_eq!(TASKS.len(), 5);
    }
}
