# GPN Desktop (Tauri)

Windows desktop GPN client — Tauri 2 (Rust backend + vanilla TS web UI).

- Watches game executables and polls their remote server IPs (tasklist + netstat on Windows)
- Resolves game display name + icon from the exe's version info (PowerShell, cached)
- Pushes detected targets to your GPN VPS (`POST /api/targets`, Bearer token)
- System tray icon: Open Dashboard / Quit, left-click shows the window
- ExitLag-style dark dashboard

## Dev

Requires Rust (stable) + Bun.

```bash
bun install
bun run tauri dev
```

On Linux, the watcher uses `ps`/`ss` and meta resolution falls back to filename.

## Build

```bash
bun run tauri build          # NSIS installer in src-tauri/target/release/bundle/
```

CI builds the Windows NSIS installer on every push (artifact `GPNClient-windows-installer`).

## Config

Stored in the app data dir (`%APPDATA%/com.gpn.client/gpn.config.json`):

```json
{
  "gpn_server_url": "https://gpn.example.com",
  "gpn_token": "shared-secret",
  "poll_interval_ms": 2000,
  "games": ["D:\\...\\AION2.exe"],
  "game_ports": []
}
```

## Server API expected

- `POST /api/targets` — `{ "targets": [{"ip","port","proto"}], "ts" }`, `Authorization: Bearer <token>`
- `GET /api/health` — used by "Test connection"
