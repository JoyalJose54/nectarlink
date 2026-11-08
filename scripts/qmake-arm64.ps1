# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Prepares a qmake that cross-compiles for Windows ARM64 from an x64 host
    and prints its path, for use as the QMAKE environment variable.

.DESCRIPTION
    Qt's ARM64 kit ships host-qmake.bat, but cxx-qt runs qmake with an empty
    environment, which a .bat file can't survive. Instead, this copies the x64
    host qmake (and the Qt6Core.dll it needs) into a cache folder next to a
    qt.conf describing the ARM64 kit.
    Headers and libraries then come from the ARM64 kit; moc, rcc and
    qmlcachegen run from the x64 kit.

.EXAMPLE
    $env:QMAKE = ./scripts/qmake-arm64.ps1
    cargo build --release --target aarch64-pc-windows-msvc -p s1-qt-rust
#>
param(
    [string]$QtRoot = "C:\Qt\6.12.0",
    [string]$OutDir = (Join-Path $env:LOCALAPPDATA "Nectarlink\dev\qmake-arm64")
)
$ErrorActionPreference = "Stop"

$hostKit = Join-Path $QtRoot "msvc2022_64"
$targetKit = Join-Path $QtRoot "msvc2022_arm64"
foreach ($kit in $hostKit, $targetKit) {
    if (-not (Test-Path (Join-Path $kit "bin"))) { throw "Qt kit not found: $kit" }
}

New-Item -ItemType Directory -Force $OutDir | Out-Null
# Copy host (x64) tools and runtime DLLs into the cache folder next to qt.conf.
# qmake/qtpaths link Qt dynamically and may run with an isolated PATH, so their
# DLLs must sit next to them.
$toolsToCopy = @(
    "qmake6.exe", "qmake.exe",
    "qtpaths6.exe", "qtpaths.exe",
    "qmlimportscanner.exe", "windeployqt.exe",
    "Qt6Core.dll", "Qt6Qml.dll"
)
foreach ($file in $toolsToCopy) {
    $src = Join-Path $hostKit "bin\$file"
    if (Test-Path $src) {
        Copy-Item $src (Join-Path $OutDir $file) -Force
    }
}
if ((Test-Path (Join-Path $OutDir "qmake6.exe")) -and (-not (Test-Path (Join-Path $OutDir "qmake.exe")))) {
    Copy-Item (Join-Path $OutDir "qmake6.exe") (Join-Path $OutDir "qmake.exe") -Force
}
if ((Test-Path (Join-Path $OutDir "qtpaths6.exe")) -and (-not (Test-Path (Join-Path $OutDir "qtpaths.exe")))) {
    Copy-Item (Join-Path $OutDir "qtpaths6.exe") (Join-Path $OutDir "qtpaths.exe") -Force
} elseif ((Test-Path (Join-Path $OutDir "qtpaths.exe")) -and (-not (Test-Path (Join-Path $OutDir "qtpaths6.exe")))) {
    Copy-Item (Join-Path $OutDir "qtpaths.exe") (Join-Path $OutDir "qtpaths6.exe") -Force
}

# Same layout as the kit's target_qt.conf, with absolute paths.
$t = $targetKit -replace '\\', '/'
$h = $hostKit -replace '\\', '/'
@"
[Paths]
Prefix=$t
Documentation=doc
Headers=include
Libraries=lib
LibraryExecutables=bin
Binaries=bin
Plugins=plugins
QmlImports=qml
ArchData=.
Data=.
Translations=translations
Examples=examples
Tests=tests
Settings=etc/xdg
HostPrefix=$h
HostBinaries=bin
HostLibraries=lib
HostLibraryExecutables=bin
HostData=$t
Sysroot=
SysrootifyPrefix=false
TargetSpec=win32-arm64-msvc
HostSpec=win32-msvc
"@ | Set-Content -Encoding ascii (Join-Path $OutDir "qt.conf")

if (Test-Path (Join-Path $OutDir "qmake6.exe")) {
    Join-Path $OutDir "qmake6.exe"
} else {
    Join-Path $OutDir "qmake.exe"
}
