# GPN Desktop (Tauri)

Windows desktop GPN client — Tauri 2 (Rust backend + vanilla TS web UI).

- Watches game executables (matched by file name) and detects their servers: established TCP via
  netstat, and **TCP + UDP via ETW** (Microsoft-Windows-Kernel-Network, per-process, no driver)
- **Learn mode**: records every public server a game talks to (traffic, play sessions, AS / BGP
  prefix via Team Cymru whois), you tick the real game servers and save a **profile**
- Resolves game display name + icon from the exe's version info (PowerShell, cached)
- User login/register against the GPN server, WireGuard split-tunnel to the VPS, game routes
  pushed to the server (see *Tunnel* below)
- System tray icon: Open Dashboard / Quit, left-click shows the window
- ExitLag-style dark dashboard

## Learn mode & profiles

1. Watch the game, open the app **as administrator** (ETW needs it; the app offers “Relaunch as
   admin” when it can't start the trace — or add your user to *Performance Log Users*).
2. Pick the game under *Learn Mode*, press **Start learning**, play a few sessions.
3. Hosts are ranked by traffic and how many sessions they appeared in. Pre-selected = sustained
   traffic and not DNS/NTP-style chatter. For each host choose `/32` or its whole announced prefix
   (shared-cloud prefixes are flagged). **Save profile**, **Export JSON** for other tools.

Data lives in the app data dir: `learned.json` (observations), `profiles.json`, `profiles/<exe>.json`.
Profiles reject non-public addresses and prefixes wider than /16. Set `GPN_ETW_DEBUG=1` to log raw
ETW events if capture looks wrong.

## Tunnel

Requires **WireGuard for Windows** (`C:\Program Files\WireGuard`) and the app running **as administrator**
(installing the tunnel service). Log in (or register) in *Account & Tunnel*; the session
(access + refresh token, single-flight refresh) is stored in `gpn.config.json`, which is restricted to
your user + SYSTEM. A shared machine token in Settings still works as a fallback when logged out.

- A persistent WG keypair is created once (`wg.exe genkey/pubkey`) and registered with
  `POST /api/register`; targets go to `POST /api/targets` with the `x-gpn-peer` header.
- The split-tunnel config (`AllowedIPs` = only game routes, no DNS) is written to
  `Tunnel Configs\gpn.conf` and installed with `wireguard.exe /installtunnelservice`.
  Route changes reinstall the service (short blip); changes are debounced and strictly serialized.
- **Pre-routing**: when a watched game's process appears, the CIDRs of its saved profile are routed
  immediately, before the game connects (re-routing an open socket breaks the session). Process
  polling is every `poll_interval_ms`, so a game that connects in its very first moments can still
  win the race; one already running when the app starts is routed late. Live hosts seen via
  ETW/netstat are added as /32 (unless a profile CIDR covers them).
- Routing is per CIDR: a profile's protocol/ports are informational and not enforced in the tunnel.
- The server only accepts public IPv4 /32 targets, so only /32 hosts (profile `/32` entries and live
  hosts, max 60) are synced to it; wider profile prefixes are routed client-side only.
- Watchdog (30s): if the latest handshake (`wg show … latest-handshakes` is a unix timestamp) is
  older than 3 min, or none occurred since start, the service is reinstalled.
- The tunnel is torn down after ~60s with no game traffic (`tunnel_teardown_when_idle`), on logout and on quit.
  A tunnel left running by a crash is adopted on start.

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
  "gpn_token": "",
  "tunnel_teardown_when_idle": true,
  "poll_interval_ms": 2000,
  "games": ["D:\\...\\AION2.exe"],
  "game_ports": []
}
```

The config also holds `wg_private_key` and `auth` (the session); don't share the file.

## Server API used

- `POST /api/auth/{register,login,refresh,logout}`
- `POST /api/register` `{publicKey,name}` -> `{ip,serverPublicKey,endpoint}`
- `POST /api/targets` `{targets:[{ip}]}` with `Authorization: Bearer …` and `x-gpn-peer: <wg public key>`
- `GET /api/health` — used by "Test connection"
