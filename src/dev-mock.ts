// Dev-only: fake Tauri backend so the UI can be developed and screenshotted in a plain browser.
// Loaded only by `bun run dev` when no Tauri runtime is present. Pick a scenario with ?scene=
//   running (default) | idle | empty | signedout | denied | error
/* eslint-disable @typescript-eslint/no-explicit-any */

const scene = new URLSearchParams(location.search).get("scene") ?? "running";
const w = window as any;

const iconSvg = (c1: string, c2: string, ch: string) =>
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    `<svg xmlns='http://www.w3.org/2000/svg' width='64' height='64'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'><stop stop-color='${c1}'/><stop offset='1' stop-color='${c2}'/></linearGradient></defs><rect width='64' height='64' rx='14' fill='url(#g)'/><text x='32' y='43' font-size='30' font-family='Segoe UI' font-weight='700' fill='white' text-anchor='middle'>${ch}</text></svg>`
  );

const AION = "D:\\SteamLibrary\\steamapps\\common\\AION2\\Aion2\\Binaries\\Win64\\AION2.exe";
const OTHER = "C:\\Games\\Valorant\\live\\VALORANT.exe";

const state: any = {
  procs: [],
  targets: [],
  watched: [AION, OTHER],
  gpnServerUrl: "https://gpn.example.com",
  meta: {
    [AION]: { name: "AION 2", company: "NCSOFT", icon: iconSvg("#6366f1", "#22d3ee", "A"), source: "versioninfo" },
    [OTHER]: { name: "VALORANT", company: "Riot Games", icon: iconSvg("#ef4444", "#f97316", "V"), source: "versioninfo" },
  },
  etw: { state: "running", message: "capturing per-process network flows" },
  learning: false,
  auth: { username: "akmal", role: "admin" },
  hasToken: false,
  insecureUrl: false,
  tunnel: { registered: true, vpnIp: "10.66.0.2", tunnelUp: true, allowedIps: ["203.0.113.50", "198.51.100.7", "52.84.12.9"], handshakeAgeSecs: 38, dup: { sent: 18200, received: 18100, downPairs: 17650, downSingles: 380 }, lastError: null },
  dup: { enabled: false, delayMs: 3 },
};

const running = { pid: 4242, name: "AION2.exe", game: "aion2.exe" };
if (scene === "running") {
  state.procs = [running];
  state.targets = [
    { ip: "203.0.113.50", port: 7777, proto: "udp" },
    { ip: "198.51.100.7", port: 443, proto: "tcp" },
    { ip: "52.84.12.9", port: 443, proto: "tcp" },
    { ip: "34.120.5.5", port: 443, proto: "tcp" },
  ];
  state.learning = true;
} else if (scene === "idle") {
  state.tunnel = { ...state.tunnel, tunnelUp: false, allowedIps: [], handshakeAgeSecs: null };
} else if (scene === "empty") {
  state.watched = [];
  state.meta = {};
  state.tunnel = { ...state.tunnel, tunnelUp: false, allowedIps: [], vpnIp: null, registered: false };
} else if (scene === "signedout") {
  state.auth = null;
  state.tunnel = { ...state.tunnel, tunnelUp: false, allowedIps: [], vpnIp: null, registered: false };
} else if (scene === "denied") {
  state.etw = { state: "denied", message: "ETW needs an elevated process (run as administrator)" };
  state.tunnel = { ...state.tunnel, tunnelUp: false, allowedIps: [], lastError: "could not install the tunnel service (is the app running as Administrator?)" };
} else if (scene === "error") {
  state.procs = [running];
  state.tunnel = { ...state.tunnel, tunnelUp: false, allowedIps: [], lastError: "register failed: HTTP 403 peer limit reached (5)" };
}

const learnData: Record<string, any> = {
  "aion2.exe": {
    learning: state.learning,
    sessions: 3,
    entries: [
      { ip: "203.0.113.50", proto: "udp", ports: { 7777: 91234, 7778: 120 }, bytes_out: 8_400_000, bytes_in: 31_200_000, sessions: 3, total_bytes: 39_600_000, suggested: true, asn: { asn: 64500, prefix: "203.0.113.0/24", cc: "KR", name: "NCSOFT-KR NC Soft Corp" } },
      { ip: "198.51.100.7", proto: "tcp", ports: { 443: 4200 }, bytes_out: 900_000, bytes_in: 3_100_000, sessions: 3, total_bytes: 4_000_000, suggested: true, asn: { asn: 64500, prefix: "198.51.100.0/22", cc: "KR", name: "NCSOFT-KR NC Soft Corp" } },
      { ip: "52.84.12.9", proto: "tcp", ports: { 443: 310 }, bytes_out: 40_000, bytes_in: 2_300_000, sessions: 2, total_bytes: 2_340_000, suggested: true, asn: { asn: 16509, prefix: "52.84.0.0/15", cc: "US", name: "AMAZON-02 Amazon.com, Inc." } },
      { ip: "34.120.5.5", proto: "tcp", ports: { 443: 22 }, bytes_out: 2_000, bytes_in: 14_000, sessions: 1, total_bytes: 16_000, suggested: false, asn: { asn: 396982, prefix: "34.96.0.0/14", cc: "US", name: "GOOGLE-CLOUD-PLATFORM" } },
      { ip: "1.1.1.1", proto: "udp", ports: { 53: 40 }, bytes_out: 3_000, bytes_in: 6_000, sessions: 3, total_bytes: 9_000, suggested: false, asn: null },
    ],
  },
};

// synthetic timeline: 3 sessions; a second game server shows up mid-session 3, after the profile was saved
{
  const MIN = 60_000, now = Date.now();
  const starts = [now - 190 * MIN, now - 95 * MIN, now - 32 * MIN];
  const d = learnData["aion2.exe"];
  d.session_starts = starts.map((t, i) => ({ session: i + 1, started: t }));
  const ep = (s: number, a: number, b: number) => ({ session: s, first: starts[s - 1] + a * MIN, last: starts[s - 1] + b * MIN });
  const by: Record<string, any[]> = {
    "203.0.113.50": [ep(1, 0, 28), ep(2, 1, 40), ep(3, 0, 11)],
    "198.51.100.7": [ep(1, 0, 27), ep(2, 0, 41), ep(3, 0, 31)],
    "52.84.12.9": [ep(1, 3, 5), ep(1, 14, 15), ep(2, 6, 8), ep(3, 20, 22)],
    "34.120.5.5": [ep(3, 9, 10)],
    "1.1.1.1": [ep(1, 0, 1)],
  };
  for (const e of d.entries) { e.episodes = by[e.ip] ?? []; e.first_seen = now - 190 * MIN; e.last_seen = now; }
  d.entries.push({ ip: "203.0.113.77", proto: "tcp", ports: { 13328: 20300 }, bytes_out: 600_000, bytes_in: 1_900_000, sessions: 1, total_bytes: 2_500_000, suggested: true,
    first_seen: starts[2] + 11 * MIN, last_seen: now, episodes: [ep(3, 11, 31)], asn: { asn: 8075, prefix: "20.192.0.0/10", cc: "KR", name: "MICROSOFT-CORP-MSN-AS-BLOCK" } });
  d.sessions = 3;
}

let profiles: any[] = scene === "empty" ? [] : [
  { version: 1, game: "aion2.exe", name: "AION 2", updated: Date.now() - 24 * 60_000, entries: [
    { cidr: "203.0.113.50/32", proto: "tcp", ports: [], source: "observed", asn: 64500, as_name: "NCSOFT", note: "" },
    { cidr: "198.51.100.0/22", proto: "tcp", ports: [], source: "prefix", asn: 64500, as_name: "NCSOFT", note: "" },
    ...Array.from({ length: 337 }, (_, i) => ({ cidr: `20.${i % 250}.0.0/17`, proto: "tcp", ports: [], source: "pack", asn: 8075, as_name: "Azure Japan East", note: "AzureCloud.japaneast, Microsoft service tags 2026-10-12" })),
  ] },
  { version: 1, game: "valorant.exe", name: "VALORANT", updated: Date.now() - 86_400_000 * 3, entries: [
    { cidr: "162.249.72.0/24", proto: "udp", ports: [], source: "prefix", asn: 6507, as_name: "RIOT", note: "" },
    { cidr: "104.160.131.3/32", proto: "tcp", ports: [443], source: "observed", asn: 6507, as_name: "RIOT", note: "" },
  ] },
];

const keyOf = (g: string) => (g.split(/[\\/]/).pop() ?? g).toLowerCase();
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
let cbId = 1;

w.__TAURI_INTERNALS__ = {
  metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
  transformCallback: () => cbId++,
  unregisterCallback: () => {},
  convertFileSrc: (p: string) => p,
  invoke: async (cmd: string, args: any = {}) => {
    await wait(30);
    switch (cmd) {
      case "get_state": {
        if (state.tunnel.tunnelUp) state.tunnel.handshakeAgeSecs = (state.tunnel.handshakeAgeSecs + 3) % 120;
        return structuredClone(state);
      }
      case "watch_game":
        if (!state.watched.includes(args.exe)) state.watched.push(args.exe);
        state.meta[args.exe] = { name: args.exe.split(/[\\/]/).pop().replace(/\.exe$/i, ""), source: "filename" };
        return null;
      case "unwatch_game": state.watched = state.watched.filter((g: string) => g !== args.exe); return null;
      case "save_settings": if (args.serverUrl) state.gpnServerUrl = args.serverUrl; return null;
      case "ping_server": return "ok";
      case "set_duplication": state.dup = { enabled: args.enabled, delayMs: args.delayMs }; return null;
      case "dup_server_stats": return [17400, 410];
      case "udp_probe": await new Promise((r) => setTimeout(r, 1200)); return { sent: 200, singleLossPct: 1.5, dupLossPct: 0, avgRttMs: 27.4, jitterMs: 1.2, delayMs: 3 };
      case "auth_login":
        if (args.password === "bad") throw "invalid credentials";
        state.auth = { username: args.username, role: "user" };
        return null;
      case "auth_register": state.auth = { username: args.username, role: "user" }; return null;
      case "auth_logout": state.auth = null; return null;
      case "get_learn": return structuredClone({ ...(learnData[keyOf(args.game)] ?? { sessions: 0, entries: [] }), learning: state.learning });
      case "get_traffic": {
        const n = Math.max(10, args.windowSecs ?? 120);
        const live = state.procs.length > 0;
        const t = Date.now() / 1000;
        const series = (base: number, amp: number) => Array.from({ length: n }, (_, i) => live ? Math.max(0, Math.round(base + amp * Math.sin((t - (n - i)) / 3.1) + (Math.random() - 0.5) * base * 0.5)) : 0);
        const rx = series(38_000, 22_000), tx = series(5_500, 3_000);
        const sum = (a: number[]) => a.reduce((x, y) => x + y, 0);
        if (!live) return { active: false, started_ms: 0, total_rx: 0, total_tx: 0, peak_rx: 0, peak_tx: 0, rx_per_sec: 0, tx_per_sec: 0, series_rx: rx, series_tx: tx, hosts: [] };
        const hosts = [
          { ip: "203.0.113.50", proto: "tcp", port: 13328, rx: 84_000_000, tx: 11_000_000, last_age_ms: 400 },
          { ip: "203.0.113.77", proto: "tcp", port: 13328, rx: 21_000_000, tx: 3_000_000, last_age_ms: 900 },
          { ip: "198.51.100.7", proto: "tcp", port: 443, rx: 3_100_000, tx: 900_000, last_age_ms: 22_000 },
          { ip: "52.84.12.9", proto: "tcp", port: 443, rx: 2_200_000, tx: 100_000, last_age_ms: 130_000 },
        ];
        return { active: true, started_ms: Date.now() - 26 * 60_000, total_rx: 110_300_000, total_tx: 15_000_000, peak_rx: 310_000, peak_tx: 44_000,
          rx_per_sec: sum(rx.slice(-3)) / 3, tx_per_sec: sum(tx.slice(-3)) / 3, series_rx: rx, series_tx: tx, hosts };
      }
      case "get_path": {
        const live = state.procs.length > 0;
        const wobble = (base: number, amp: number, loss: number) => Array.from({ length: 60 }, (_, i) => (Math.random() * 100 < loss ? null : Math.round((base + amp * Math.sin(i / 4) + (Math.random() - 0.5) * amp) * 10) / 10));
        const stats = (samples: (number | null)[], method: string) => {
          const ok = samples.filter((x): x is number => x !== null);
          const avg = ok.reduce((a, b) => a + b, 0) / (ok.length || 1);
          const jit = ok.slice(1).reduce((a, v, i) => a + Math.abs(v - ok[i]), 0) / Math.max(1, ok.length - 1);
          return { method, last: samples[samples.length - 1], avg: Math.round(avg * 10) / 10, min: Math.min(...ok), max: Math.max(...ok), jitter: Math.round(jit * 10) / 10, lossPct: Math.round((samples.length - ok.length) / samples.length * 1000) / 10, samples };
        };
        if (scene === "empty" || scene === "signedout") return { tunnelUp: false, endpoint: null, a: null, servers: [], serversSupported: true, game: null };
        const a = stats(wobble(23, 5, 1), "icmp");
        const servers = live && state.tunnel.tunnelUp
          ? [{ ip: "203.0.113.50", port: 13328, ...stats(wobble(41, 7, 0), "tcp") }, { ip: "203.0.113.77", port: 13328, ...stats(wobble(96, 38, 7), "tcp") }]
          : [];
        return { tunnelUp: state.tunnel.tunnelUp, endpoint: "198.51.100.10:51820", a, servers, serversSupported: true, game: live ? "aion2.exe" : null };
      }
      case "reset_traffic": return null;
      case "set_learning": state.learning = !!args.enabled; return null;
      case "clear_learn": delete learnData[keyOf(args.game)]; return null;
      case "save_profile": {
        const p = { version: 1, game: args.game.split(/[\\/]/).pop().toLowerCase(), name: state.meta[args.game]?.name ?? args.game, updated: Date.now(), entries: args.entries };
        profiles = [...profiles.filter((x) => x.game !== p.game), p];
        return p;
      }
      case "get_profiles": return structuredClone(profiles);
      case "remove_pack": { const p = profiles.find((x) => x.game === args.game); if (!p) return 0; const n = p.entries.filter((e: any) => e.source === "pack").length; p.entries = p.entries.filter((e: any) => e.source !== "pack"); return n; }
      case "delete_profile": profiles = profiles.filter((p) => p.game !== args.game); return null;
      case "export_profile": return `C:\\Users\\you\\AppData\\Roaming\\com.gpn.client\\profiles\\${args.game}.json`;
      case "etw_restart": state.etw = { state: "running", message: "capturing per-process network flows" }; return state.etw;
      case "relaunch_as_admin": return null;
      case "plugin:window|is_maximized": return false;
      case "plugin:dialog|open": return "C:\\Games\\Example\\Example.exe";
      case "plugin:event|listen": return cbId++;
      default: return null; // window/event plugin calls
    }
  },
};
w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
export {};
