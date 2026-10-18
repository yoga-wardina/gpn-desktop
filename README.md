# GPN Desktop (Tauri)

Windows desktop client for the Game Private Network: it detects the servers a game talks to and routes
**only that traffic** through a WireGuard tunnel to your own VPS (see `gpn-server`), which NATs it onward.
Tauri 2 (Rust backend + vanilla TypeScript UI), frameless desktop-style window.

## Features

- **Game watcher**: matches game executables by file name; established TCP via netstat, and **TCP + UDP via
  ETW** (Microsoft-Windows-Kernel-Network, per process, no driver).
- **Traffic monitor**: live per-game throughput graph, totals, peaks and per-host breakdown (no learn mode
  needed).
- **Connection path**: latency / jitter / packet loss for both legs, drawn as a path
  `PC → GPN server → game server`:
  - *PC → GPN server*: ICMP echo (TCP-connect fallback) to the VPS, measured locally.
  - *GPN server → game server*: measured **by the VPS** through `POST/GET /api/probe` (needs the updated
    server; the dashboard says "update server" otherwise).
- **Learn mode + profiles**: records every public server a game talks to (traffic, play sessions, AS / BGP
  prefix via Team Cymru whois), with a per-session **Timeline**. You tick the real game servers and save a
  **profile**; export as JSON.
- **Packet loss protection (experimental)**: sends every tunnel packet twice to the server's duplication relay so
  random loss on your own connection never reaches the game; see *Packet duplication*.
- **Sticky routes + live route updates**: see *Tunnel*. Prevents the "kicked when moving from an instance to
  the open world" problem.
- **Range packs**: a profile can carry a bundled range list (e.g. a whole cloud region) that is pre-routed
  whenever the game runs; survives re-saving the reviewed servers.
- Game name and 256px icon resolved from the exe's version info (cached, re-resolved when stale).
- Account login / register against the GPN server, system tray (Open / Quit). *Settings → Window*:
  **minimize to tray** (default on) and **close hides to tray** (default off; off = the X quits and stops the tunnel).
- Themes: Toxic (default), Crimson, Graphite, Midnight, Forest, Light, System; normal (1280×820, fixed) or
  full-window mode. Window position is remembered.

## Defaults

First run is seeded from `src-tauri/default-config.json` (no secrets): the GPN server URL, poll interval,
a starter game list and `tunnel_teardown_when_idle`. Edit that file to change the defaults baked into a build;
missing fields in an existing `gpn.config.json` fall back to it.

## Learn mode & profiles

1. Run the app **as administrator** (ETW needs it; the release exe asks for elevation itself via its
   manifest; in debug builds use an elevated terminal, or add your user to *Performance Log Users*).
2. Pick the game under *Learn*, press **Start learning**, play a few sessions (including instances / zone
   changes: MMOs hand you between servers).
3. Hosts are ranked by traffic and by how many sessions they appeared in. Pre-selected = sustained traffic and
   not DNS/NTP-style chatter. For each host choose `/32` or its whole announced prefix (shared-cloud prefixes
   are flagged). **Save profile**.

Data lives in `%APPDATA%\com.gpn.client\`: `learned.json` (observations), `profiles.json`,
`profiles/<exe>.json` (exports). Profiles reject non-public addresses and prefixes wider than /16 (max 500
entries). Set `GPN_ETW_DEBUG=1` to log raw ETW events if capture looks wrong.

### Range packs

Games that move you between many servers in one cloud region (e.g. AION2 → Azure Japan East) can't be fully
learned, and an established TCP connection cannot be moved onto the tunnel afterwards. A pack pre-routes the
region instead:

- Entries with `"source": "pack"` in `profiles.json` (one per CIDR, `/16` or narrower, public only).
- They are routed client-side only and **not** sent to the server's target list.
- Shown on the Profiles page as one chip with a **Remove** button; re-saving the reviewed servers keeps them.
- Microsoft publishes the ranges as the weekly *Azure IP Ranges and Service Tags – Public Cloud* JSON
  (`AzureCloud.japaneast`); they go stale, so refresh occasionally. There is no importer UI yet: the pack is
  written into `profiles.json` by script.
- Trade-off: while the game runs, all traffic to those ranges goes through the VPS.

## Tunnel

Requires **WireGuard for Windows** (`C:\Program Files\WireGuard`) and the app running **as administrator**.
Log in (or register) in *Account & Tunnel*; the session (access + refresh token, single-flight refresh) is
stored in `gpn.config.json`, restricted to your user + SYSTEM. A shared machine token still works as a
fallback when logged out.

- A persistent WG keypair is created once (`wg.exe genkey/pubkey`) and registered with `POST /api/register`;
  targets go to `POST /api/targets` with the `x-gpn-peer` header.
- The split-tunnel config (`AllowedIPs` = only game routes, no DNS) is written to `Tunnel Configs\gpn.conf`
  and installed with `wireguard.exe /installtunnelservice`.
- **Pre-routing**: when a watched game's process appears, its profile CIDRs (reviewed hosts + packs) are routed
  immediately, before it connects. Process polling runs every `poll_interval_ms`, so a game that connects in
  its very first moments can still win the race, and one already running when the app starts is routed late.
- **Sticky live hosts**: servers seen live (ETW/netstat) are added as /32 and kept for 60 s after the last
  sighting (cap 40), only while a game is running, unless a profile CIDR already covers them.
- **Live route updates**: adding or removing routes uses `wg set peer … allowed-ips` plus
  `netsh interface ipv4 add/delete route … interface=gpn` and verifies the result, so existing connections are
  **not dropped**. If verification fails it falls back to reinstalling the service (a 1–2 s blip that kicks
  connected games). Changes are debounced and strictly serialized.
- Routing is per CIDR: a profile's protocol/ports are informational and not enforced.
- The server only accepts public IPv4 /32 targets, so only reviewed /32 hosts and live hosts (max 60) are
  synced to it; wider prefixes and pack ranges are routed client-side only.
- Watchdog (30 s): if the latest handshake is older than 3 min, or none occurred since start, the service is
  reinstalled.
- The tunnel is torn down after ~60 s with no game traffic (`tunnel_teardown_when_idle`), on logout and on
  quit. A tunnel left running by a crash is adopted on start.

## Packet duplication

For links with random packet loss between you and the VPS. WireGuard is pointed at a small local UDP proxy,
which sends each datagram to the server's relay twice (default 3 ms apart); the server's WireGuard drops the
repeat via replay protection, and the relay duplicates replies back. A packet is lost only if both copies are.

1. Server: set `dupPort` (e.g. `51821`) in `server.config.json`, `ufw allow 51821/udp`, restart (see its README).
2. App: *Settings → Packet loss protection → Run test* sends 200 UDP probes and reports loss with one copy vs. two.
   Only turn it on if the test shows real loss and a clear improvement (ICMP loss can be rate-limiting, not real).
3. Enable it; it applies the next time the tunnel starts (a running tunnel is never restarted, which would
   kick games). While active, the Dashboard has a **Packet loss protection** card: ~packets rescued this
   session, link loss per direction (downstream counted in the app, upstream by the server, so it needs the
   updated server), packets sent/received, and a link-loss-over-time chart. A packet that arrives with only
   one of its two copies means one copy was lost; about half of those were the first copy, which is what the
   backup copy rescued (hence the "~"). Both-copies-lost packets are invisible by definition.

Costs double upstream/downstream bandwidth on the PC ↔ VPS leg only. It cannot help when both copies are lost
together (a congested hop); spacing them further apart helps a little. `packet_duplication` and
`duplication_delay_ms` in `gpn.config.json` hold the settings. A tunnel left running by a crash while
duplication was on is removed at the next start (its local proxy died with the app).

## Known limitations

- Entering a server outside your profile and packs (a new region or another cloud) can still cause one
  reconnect, because that connection started before its route existed.
- Pack ranges go stale as Microsoft updates them; the server does no egress filtering (personal use).
- The Learn timeline only has data from the build that introduced it onward.
- A game using a game-specific VPN/anti-cheat policy may object to VPN use; check the publisher's terms.

## Dev

Requires Rust (stable) + Bun (or Node).

```bash
bun install
bun run tauri dev
```

The UI also runs standalone in a browser with a mock backend (`bun run dev`, then
`?scene=running|idle|empty|signedout|denied|error`). Tests: `cargo test` in `src-tauri`.

## Build

```bash
bun run tauri build          # NSIS installer in src-tauri/target/release/bundle/
```

The release exe requests administrator rights (UAC) on launch. If the build fails on a stale path, run
`cargo clean --release` in `src-tauri`. CI builds the Windows NSIS installer on every push.

## Config

`%APPDATA%\com.gpn.client\gpn.config.json` (defaults from `default-config.json`):

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

The file also holds `wg_private_key` and `auth` (the session); don't share it.

## Server API used

- `POST /api/auth/{register,login,refresh,logout}`
- `POST /api/register` `{publicKey,name}` -> `{ip,serverPublicKey,endpoint}`
- `POST /api/targets` `{targets:[{ip}]}` with `Authorization: Bearer …` and `x-gpn-peer: <wg public key>`
- `POST/GET /api/probe` — server-side latency/loss to game servers on your target list
- `GET /api/health` — used by "Test connection"; its `dup` field names the duplication relay
