# Elevated over USB

> Status: draft (v0). Part of [the protocol](v0.md). Breaking changes are
> allowed until v1. License: CC BY 4.0.

Elevated runs a small helper on the phone as Android's shell user
(`app_process`, from the app's own APK). The phone normally starts it through
its own wireless debugging. A phone plugged into a PC with USB debugging
allowed can instead ask that PC to start it, the way scrcpy starts its server.

## 1. Capability

| Capability | Offered by | Meaning |
|---|---|---|
| `elevated.usb` | PC | Can start a USB-connected phone's helper over adb |

A PC offers it while it watches adb for USB phones.

## 2. Messages

```
t = "elevated.usb.start"   id = n   b = { apk: text, token: text, grants: [text] }
t = "elevated.usb.ready"   id = n   b = { port: uint }
```

The phone sends `elevated.usb.start`. The PC:

1. Refuses unless the request is valid (§3) and the phone is connected to it
   over USB, through that phone's own adb serial.
2. Runs each grant in the phone's shell, waiting up to 10 seconds for each.
3. Runs `CLASSPATH=<apk> app_process /system/bin
   app.nectarlink.android.elevated.InputServer <token>` and waits up to 15
   seconds for its first line, `ready <port>`.
4. Answers `elevated.usb.ready` with that port, or an error.

The phone connects to the helper on `127.0.0.1:<port>` with the token, as when
it starts the helper itself. The helper runs while the PC holds its adb shell:
until the phone asks again, the USB tunnel ends, or the PC's node stops.

## 3. What the PC accepts

The PC runs nothing but the app's own helper and grants, and only words the
shell reads as single words (letters, digits and `._/=~+-:`):

- `apk`: a path under `/data/app/` ending in `/base.apk`, containing
  `app.nectarlink`, without `..`, at most 512 bytes.
- `token`: 32 to 64 hexadecimal characters.
- `grants`: at most 8, each one of:
  - `pm grant app.nectarlink android.permission.<NAME>`
  - `appops set app.nectarlink <OP> allow` (`OP` in capitals and `_`)
  - `cmd notification allow_dnd app.nectarlink`
  - `cmd notification allow_listener app.nectarlink/<component>`
