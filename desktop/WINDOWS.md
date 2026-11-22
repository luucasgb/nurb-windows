# nurb on Windows

The desktop app builds, provisions and runs on Windows 10/11 x64. Every macOS
path is unchanged; the Windows code sits behind `cfg(windows)`.

## Building

Prerequisites: Rust (MSVC toolchain), Visual Studio Build Tools with the C++
workload and a Windows 11 SDK, Node 22+, uv, and Git for Windows (the npm
scripts call `sh`).

```powershell
# from the repo root, in PowerShell
npm ci
$env:Path += ";C:\Program Files\Git\usr\bin"   # so the npm scripts find sh
npm run tauri build --workspace desktop          # -> src-tauri/target/release/bundle/nsis/nurb_<v>_x64-setup.exe
```

`scripts/stage.sh` downloads the pinned `uv.exe` sidecar instead of the darwin
pair when it runs on Windows. `tauri.windows.conf.json` swaps the bundle to a
per-user NSIS installer and keeps the native title bar.

## The agent sandbox

The agent is never run without the sandbox. On Windows, nurb.exe itself plays
the role of `sandbox-exec`: `acp/sandbox/windows.rs` turns an adapter launch
into `nurb.exe --nurb-sandbox --write <root>... -- <program> <args>`, and that
helper starts the program under a write-restricted token. This is the same
model as Codex's unelevated Windows sandbox:

- Restricting SIDs: a SID only nurb carries (derived from the user's SID, so it
  is stable across launches and reinstalls), the logon session, and Everyone.
  Token flags `DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED`.
- nurb's SID gets inheritable modify rights on the same roots the Seatbelt
  profile opens: the project, the engine's home, a private temp folder (handed
  to the child as TEMP/TMP), npm and uv caches, `~/.config/nurb`, and the
  agents' own state folders, plus `~/.claude.json` by name.
- A write anywhere else fails at the kernel ("Access is denied"), in the
  agent's own transcript, exactly as on macOS. Reads and network are open, as
  on macOS.
- The child runs in a job object that is killed when the helper's handle
  closes, so the app's process-tree kill takes every command the agent started.
- If the helper cannot set the sandbox up, it refuses to start the program
  (exit 126). It never falls back to running unsandboxed.

`cargo test` on Windows runs the security property as tests: a write inside
the granted root succeeds, a write beside it fails, a write into the home
folder fails, a child started inside inherits the walls, reads work.

Known differences from macOS:

- Reads of Documents, Desktop and Downloads are not refused. On macOS those
  rules exist to avoid privacy prompts, which Windows does not have, and a
  write-restricted token cannot restrict reads.
- The grants are ACL entries, so they persist on the folders. They name only
  nurb's SID, which no other process carries.
- The build worker's own sandbox (`engine/src/nurb/sandbox.py`) stays
  macOS-only, the same as on Linux today.

## Other Windows behavior

- Processes: no process groups or signals, so a "group" is the process tree
  under the child's pid (with creation-time checks so a recycled pid is never
  mistaken for a child), and every signal ends that tree (`platform.rs`).
- Provisioning goes to `%LOCALAPPDATA%\dev.nurb.desktop` (not the roaming
  profile): the win-x64 Node zip (checksum-pinned) unpacked with System32's
  curl and tar, the venv's `Scripts\`, and npm bins resolved from each
  package's manifest, since `.bin\` holds shell shims on Windows.
- PATH comes from the registry (machine, then user) ahead of the inherited one,
  so a CLI installed since sign-in is found. CLIs are found by `.exe`.
- The Gemini API key lives in Windows Credential Manager.

## Not done yet

- Signing: no Authenticode signature (SmartScreen will warn) and no updater
  artifacts (`createUpdaterArtifacts` is off in the Windows config until the
  release has the signing key).
- `nurb://` share links do not open the app (no URL scheme registration).
- HEIC/TIFF photos are not converted (macOS uses sips); PNG and JPEG attach
  as usual. Attachment thumbnails need the same converter.
- Cosmetic: the header keeps the gap macOS uses for its window buttons, and
  tooltips still say ⌘.
