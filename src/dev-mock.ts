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
  tunnel: { registered: true, vpnIp: "10.66.0.2", tunnelUp: true, allowedIps: ["203.0.113.50", "198.51.100.7", "52.84.12.9"], handshakeAgeSecs: 38, lastError: null },
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

let profiles: any[] = scene === "empty" ? [] : [
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
      case "auth_login":
        if (args.password === "bad") throw "invalid credentials";
        state.auth = { username: args.username, role: "user" };
        return null;
      case "auth_register": state.auth = { username: args.username, role: "user" }; return null;
      case "auth_logout": state.auth = null; return null;
      case "get_learn": return structuredClone({ ...(learnData[keyOf(args.game)] ?? { sessions: 0, entries: [] }), learning: state.learning });
      case "set_learning": state.learning = !!args.enabled; return null;
      case "clear_learn": delete learnData[keyOf(args.game)]; return null;
      case "save_profile": {
        const p = { version: 1, game: args.game.split(/[\\/]/).pop().toLowerCase(), name: state.meta[args.game]?.name ?? args.game, updated: Date.now(), entries: args.entries };
        profiles = [...profiles.filter((x) => x.game !== p.game), p];
        return p;
      }
      case "get_profiles": return structuredClone(profiles);
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
