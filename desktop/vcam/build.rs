// SPDX-License-Identifier: GPL-3.0-or-later
//! Gives the DLL its version information (what Explorer shows, and what code
//! signing checks: the product name and version must match the app's).

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let version = std::env::var("CARGO_PKG_VERSION").expect("version");
    let numbers: Vec<&str> = version.split(['.', '-', '+']).take(3).collect();
    let numeric = format!("{},0", numbers.join(","));
    // No #include: numeric constants keep rc.exe off the SDK headers.
    // 0x40004 is VOS_NT_WINDOWS32, 0x2 is VFT_DLL.
    let rc = format!(
        r#"1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x2
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "The NectarLink contributors"
      VALUE "FileDescription", "NectarLink virtual camera"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "nectarlink_vcam"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "nectarlink_vcam.dll"
      VALUE "ProductName", "NectarLink"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc_path = out.join("nectarlink_vcam.rc");
    if std::fs::read_to_string(&rc_path).ok().as_deref() != Some(rc.as_str()) {
        std::fs::write(&rc_path, rc).expect("write resource script");
    }
    embed_resource::compile_for_cdylib(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .expect("compile resources");
}
