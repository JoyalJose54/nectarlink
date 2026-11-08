# NectarLink PowerToys Command Palette / PowerToys Run Extension (`nectarlink-ptrun`)

`nectarlink-ptrun` is a standalone, zero-dependency Rust helper that connects Windows launcher tools (PowerToys Command Palette, PowerToys Run, Flow Launcher, Raycast for Windows) to NectarLink's [Local Authenticated API](../../docs/api.md).

## Building

```powershell
cargo build --release --manifest-path tools/powertoys-extension/Cargo.toml
```

## Prerequisites

1. Open **NectarLink → Settings → Local automation API** and turn on **Enable local automation API**.
2. NectarLink writes `%LOCALAPPDATA%\Nectarlink\api-token.json` containing the loopback port (`53320`) and a 256-bit Bearer token.

## Usage

### Querying Palette Items

```powershell
nectarlink-ptrun query ""
nectarlink-ptrun query "ring"
nectarlink-ptrun query "https://nectarlink.dev"
```

Outputs a JSON array of palette commands (`send-clipboard`, `ring`, `camera` `photo`/`scan`, `send-url`, `send-text`, plus recent timeline items).

### Invoking an Action

```powershell
nectarlink-ptrun invoke ring
nectarlink-ptrun invoke send-clipboard
nectarlink-ptrun invoke camera photo
nectarlink-ptrun invoke camera scan
nectarlink-ptrun invoke send-url "https://nectarlink.dev"
nectarlink-ptrun invoke send-text "Copied from PowerToys"
nectarlink-ptrun invoke send-file "C:\Users\Alice\Desktop\notes.pdf"
```
