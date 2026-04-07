# srcds_console

## Downloads

Grab the latest release from the [Releases page](../../releases):

| File | Architecture | Branch |
|------|-------------|--------|
| `srcds_console.exe` | x86 (32-bit) | Standard branch |
| `srcds_win64_console.exe` | x64 (64-bit) | x86-64 beta branch |

Place the appropriate file in your SRCDS game root (next to `srcds.exe`) and run it with the same arguments.

## Quick Install

Open a terminal in your SRCDS game root (the folder containing `srcds.exe`) and run:

```powershell
irm https://raw.githubusercontent.com/Pollux12/srcds_console/master/install.ps1 | iex
```

This auto-detects your architecture (x64 if `srcds_win64.exe` exists, x86 otherwise) and downloads the correct binary from the latest release.

## Quick Start

### Option A: Download a Release

1. Download the binary for your architecture from [Releases](../../releases)
2. Place it in your SRCDS game root (next to `srcds.exe`)
3. Run it with the same arguments as `srcds.exe`:

```bash
# x64 (beta branch)
srcds_win64_console.exe +maxplayers 20 -console +gamemode sandbox -port 27015 +map gm_construct +sv_setsteamaccount YOUR_GSLT_HERE -tickrate 22

# x86 (standard branch)
srcds_console.exe +maxplayers 20 -console +gamemode sandbox -port 27015 +map gm_construct +sv_setsteamaccount YOUR_GSLT_HERE -tickrate 22
```

> **Note:** Replace `YOUR_GSLT_HERE` with your [Game Server Login Token](https://steamcommunity.com/dev/managegameservers). A GSLT is required for your server to be visible online.

### Option B: Build from Source

Requires the [Rust toolchain](https://rustup.rs/).

```bash
cd srcds_patch

# x64 build (for beta branch with srcds_win64.exe)
cargo build --release --target x86_64-pc-windows-msvc
# Output: target/x86_64-pc-windows-msvc/release/srcds_console.exe → rename to srcds_win64_console.exe

# x86 build (for standard branch with srcds.exe only)
rustup target add i686-pc-windows-msvc
cargo build --release --target i686-pc-windows-msvc
# Output: target/i686-pc-windows-msvc/release/srcds_console.exe
```

### Deploy

Place the binary in your game root directory:

```
your_server/
├── srcds_console.exe          ← x86 console launcher (you add this)
├── srcds_win64_console.exe    ← x64 console launcher (you add this)
├── srcds.exe                  (original, still works)
├── srcds_win64.exe            (original, x64 branch only)
├── bin/
│   ├── dedicated.dll
│   └── win64/
│       └── dedicated.dll
└── garrysmod/
```

## Configuration

| Variable | Values | Default | Description |
|----------|--------|---------|-------------|
| `SRCDS_NO_STATUS` | `1` / `true` | unset (enabled) | Disable the bottom status bar |

The status bar shows live server info (FPS, map, players) pinned to the bottom terminal row. Disable it if your terminal doesn't support ANSI escape sequences or you prefer plain output:

```bash
# PowerShell
$env:SRCDS_NO_STATUS="1"; ./srcds_win64_console.exe +maxplayers 20 -console +gamemode sandbox -port 27015 +map gm_construct +sv_setsteamaccount YOUR_GSLT_HERE -tickrate 22

# cmd
set SRCDS_NO_STATUS=1 && srcds_win64_console.exe +maxplayers 20 -console +gamemode sandbox -port 27015 +map gm_construct +sv_setsteamaccount YOUR_GSLT_HERE -tickrate 22
```

## VSCode Integration

### Task (`.vscode/tasks.json`)

```json
{
    "version": "2.0.0",
    "tasks": [
        {
            "label": "Start SRCDS",
            "type": "shell",
            "command": "./srcds_win64_console.exe",
            "args": [
                "+maxplayers", "20",
                "-console",
                "+gamemode", "sandbox",
                "-port", "27015",
                "+map", "gm_construct",
                "+sv_setsteamaccount", "YOUR_GSLT_HERE",
                "-tickrate", "22"
            ],
            "options": { "cwd": "${workspaceFolder}" },
            "isBackground": true,
            "problemMatcher": [],
            "presentation": { "panel": "dedicated", "reveal": "always" }
        }
    ]
}
```

### Batch File

```bat
@echo off
srcds_win64_console.exe +maxplayers 20 -console +gamemode sandbox -port 27015 +map gm_construct +sv_setsteamaccount YOUR_GSLT_HERE -tickrate 22
```

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `dedicated.dll not found` | Exe not in game root | Place the exe next to `srcds.exe` |
| `Failed to load: 0x000000c1` | Architecture mismatch | Use the correct x86/x64 build for your branch |
| `Failed to load: 0x0000007e` | Missing DLLs | Ensure `bin\` directory is complete |
| `DedicatedMain not found` | Unsupported game/version | Check that `dedicated.dll` exports `DedicatedMain` |
| Server opens a new window | Using wrong exe | Make sure you're running the `_console` variant |

## How It Works

The launcher loads `dedicated.dll` directly (same as Alien Swarm's `srcds_console.exe`) and calls its `DedicatedMain` export. Before calling it, we patch the DLL's Import Address Table (IAT) in memory to hook 8 console APIs:

- **`AllocConsole` / `FreeConsole`** → no-op (prevents creating a new console window)
- **`WriteConsoleOutputCharacterA` / `WriteConsoleOutputAttribute`** → captures status text, suppresses direct screen writes (fixes grey lines)
- **`SetConsoleCursorPosition` / `SetConsoleScreenBufferSize` / `SetConsoleWindowInfo`** → no-op (prevents terminal manipulation)
- **`GetConsoleScreenBufferInfo`** → passed through (needed for buffer queries)

The status bar text is captured and rendered as a persistent bottom bar using ANSI scroll regions, so you get live FPS/map/player info without the grey line artifacts.

Normal log output (`WriteConsoleW` / `WriteFile`) is not hooked and flows to your terminal as usual.

## Comparison

| Feature | `srcds.exe` | Alien Swarm `srcds_console.exe` | This project |
|---------|-------------|---------------------|-----------------|
| Runs in your terminal | ✗ | ✓ | ✓ |
| No grey lines | N/A | ✗ | ✓ |
| x64 support | ✓ | ✗ | ✓ |
| Source available | No | No | Yes |
| Binary size | ~300 KB | ~20 KB | ~130 KB |

## CI / Releases

GitHub Actions builds both architectures on every push and creates releases on `v*` tags:

```bash
git tag v0.2.0
git push origin v0.2.0
```

## License

MIT
