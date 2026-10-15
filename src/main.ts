import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

// ───────────────────────── types ─────────────────────────

interface GameProc { pid: number; name: string; game?: string }
interface Target { ip: string; port?: number; proto: string }
interface GameMeta { name: string; company?: string; icon?: string; source: string }
interface EtwStatus { state: "off" | "running" | "denied" | "error" | "unsupported"; message: string }
interface TunnelView {
  registered: boolean;
  vpnIp: string | null;
  tunnelUp: boolean;
  allowedIps: string[];
  handshakeAgeSecs: number | null;
  lastError: string | null;
}
interface State {
  procs: GameProc[];
  targets: Target[];
  watched: string[];
  gpnServerUrl: string;
  meta: Record<string, GameMeta | null>;
  etw: EtwStatus;
  learning: boolean;
  auth: { username: string; role: string } | null;
  hasToken: boolean;
  insecureUrl: boolean;
  tunnel: TunnelView;
}
interface Asn { asn: number; prefix: string; cc: string; name: string }
interface LearnEntry {
  ip: string;
  proto: string;
  ports: Record<string, number>;
  bytes_out: number;
  bytes_in: number;
  sessions: number;
  asn: Asn | null;
  total_bytes: number;
  suggested: boolean;
}
interface LearnView { learning: boolean; sessions: number; entries: LearnEntry[] }
interface ProfileEntry {
  cidr: string;
  proto: string;
  ports: number[];
  source: "observed" | "prefix";
  asn: number | null;
  as_name: string | null;
  note: string;
}
interface Profile { version: number; game: string; name: string; updated: number; entries: ProfileEntry[] }

type View = "dashboard" | "games" | "learn" | "profiles" | "settings";
const VIEWS: View[] = ["dashboard", "games", "learn", "profiles", "settings"];

// ───────────────────────── helpers ─────────────────────────

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const esc = (s: unknown) =>
  String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c] as string));
const gameKey = (exe: string) => (exe.split(/[\\/]/).pop() || exe).toLowerCase();
const plural = (n: number, one: string, many = one + "s") => `${n} ${n === 1 ? one : many}`;
const fmtBytes = (b: number) =>
  b >= 1073741824 ? (b / 1073741824).toFixed(1) + " GB" : b >= 1048576 ? (b / 1048576).toFixed(1) + " MB" : b >= 1024 ? (b / 1024).toFixed(1) + " KB" : b + " B";
const prefixLen = (p: string) => parseInt(p.split("/")[1] ?? "32", 10);
const CLOUD_RE = /amazon|google|microsoft|azure|cloudflare|akamai|fastly|oracle|digitalocean|ovh|hetzner|linode|alibaba|tencent|vultr|cdn/i;
const errText = (e: unknown) => String(e).replace(/^Error:\s*/, "");

const ICONS: Record<string, string> = {
  dashboard: '<rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/>',
  gamepad: '<path d="M6 11h4M8 9v4M15 12h.01M18 10h.01"/><path d="M17.3 5H6.7a4 4 0 0 0-4 3.6C2.6 9.4 2 14.5 2 16a3 3 0 0 0 3 3c1 0 1.5-.5 2-1l1.4-1.4a2 2 0 0 1 1.4-.6h4.4a2 2 0 0 1 1.4.6L17 18c.5.5 1 1 2 1a3 3 0 0 0 3-3c0-1.5-.6-6.6-.7-7.3A4 4 0 0 0 17.3 5z"/>',
  crosshair: '<circle cx="12" cy="12" r="8"/><circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  sliders: '<path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1"/><circle cx="15" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="17" cy="18" r="2"/>',
  shield: '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/>',
  "shield-check": '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/><path d="M9 12l2 2 4-4"/>',
  "shield-alert": '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/><path d="M12 8v4M12 16h.01"/>',
  plug: '<path d="M9 3v5M15 3v5M6 8h12v3a6 6 0 0 1-12 0zM12 17v4"/>',
  check: '<path d="M5 12.5l4.5 4.5L19 7"/>',
  x: '<path d="M6 6l12 12M18 6L6 18"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  trash: '<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>',
  download: '<path d="M12 4v11M7 11l5 5 5-5M5 20h14"/>',
  logout: '<path d="M15 4h4a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1h-4M10 8l-4 4 4 4M6 12h10"/>',
  login: '<path d="M9 4H5a1 1 0 0 0-1 1v14a1 1 0 0 0 1 1h4M14 8l4 4-4 4M18 12H8"/>',
  radar: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="4.5"/><path d="M12 12l6-6"/>',
  info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/>',
};
const icon = (name: string) => `<svg class="i" viewBox="0 0 24 24">${ICONS[name] ?? ""}</svg>`;
function paintIcons() {
  document.querySelectorAll<SVGElement>("svg[data-icon]").forEach((el) => {
    el.setAttribute("viewBox", "0 0 24 24");
    el.innerHTML = ICONS[el.dataset.icon ?? ""] ?? "";
  });
}

function toast(msg: string, kind: "ok" | "err" | "" = "") {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.innerHTML = `${icon(kind === "ok" ? "check" : kind === "err" ? "shield-alert" : "info")}<span>${esc(msg)}</span>`;
  $("toasts").appendChild(el);
  setTimeout(() => el.remove(), kind === "err" ? 6000 : 3000);
}

const store = {
  get: (k: string) => { try { return localStorage.getItem(k); } catch { return null; } },
  set: (k: string, v: string) => { try { localStorage.setItem(k, v); } catch { /* storage unavailable */ } },
};

/** Native confirmation dialog (Esc cancels, Enter confirms). */
function confirmDialog(o: { title: string; body: string; ok: string; danger?: boolean }): Promise<boolean> {
  return new Promise((resolve) => {
    const scrim = $("scrim");
    $("dlgTitle").textContent = o.title;
    $("dlgBody").textContent = o.body;
    const ok = $<HTMLButtonElement>("dlgOk");
    ok.textContent = o.ok;
    ok.className = "btn " + (o.danger ? "rec" : "accent");
    const cancel = $<HTMLButtonElement>("dlgCancel");
    scrim.classList.add("show");
    (o.danger ? cancel : ok).focus();
    const done = (v: boolean) => {
      scrim.classList.remove("show");
      ok.onclick = cancel.onclick = null;
      document.removeEventListener("keydown", onKey, true);
      resolve(v);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(false); }
    };
    document.addEventListener("keydown", onKey, true);
    ok.onclick = () => done(true);
    cancel.onclick = () => done(false);
  });
}


// ───────────────────────── theme ─────────────────────────

type ThemePref = "system" | "toxic" | "crimson" | "graphite" | "midnight" | "forest" | "light";
const THEMES: { id: ThemePref; label: string; bg: string; card: string; accent: string }[] = [
  { id: "toxic", label: "Toxic", bg: "#0a0e0a", card: "#121812", accent: "#39ff14" },
  { id: "crimson", label: "Crimson", bg: "#101010", card: "#181818", accent: "#ff2d3d" },
  { id: "system", label: "System", bg: "#202020", card: "#f5f5f5", accent: "#60cdff" },
  { id: "graphite", label: "Graphite", bg: "#202020", card: "#2a2a2a", accent: "#60cdff" },
  { id: "midnight", label: "Midnight", bg: "#0f131b", card: "#151a24", accent: "#4cc2ff" },
  { id: "forest", label: "Forest", bg: "#131916", card: "#1a211d", accent: "#3ddc97" },
  { id: "light", label: "Light", bg: "#f5f5f5", card: "#ffffff", accent: "#0067c0" },
];
const themeQuery = window.matchMedia("(prefers-color-scheme: light)");
let themePref: ThemePref = (store.get("gpn.theme") as ThemePref) || "toxic";
if (!THEMES.some((t) => t.id === themePref)) themePref = "toxic";

function applyTheme() {
  const resolved = themePref === "system" ? (themeQuery.matches ? "light" : "graphite") : themePref;
  document.documentElement.dataset.theme = resolved;
  // the logo follows the theme's accent
  const logo = resolved === "toxic" ? "/logo-toxic.svg" : resolved === "crimson" ? "/logo-crimson.svg" : "/logo.svg";
  document.querySelectorAll<HTMLImageElement>("img.app-icon, img.mark").forEach((img) => (img.src = logo));
}
function renderThemePicker() {
  $("themePicker").innerHTML = THEMES.map(
    (t) => `<button class="swatch" role="radio" aria-checked="${t.id === themePref}" data-theme-pick="${t.id}" title="${t.label}">
      <span class="sw" style="--sw-bg:${t.bg};--sw-card:${t.card};--sw-accent:${t.accent}"></span>${t.label}</button>`
  ).join("");
}
function setThemePref(p: ThemePref) {
  themePref = p;
  store.set("gpn.theme", p);
  applyTheme();
  renderThemePicker();
}
themeQuery.addEventListener("change", () => themePref === "system" && applyTheme());

// ───────────────────────── app state ─────────────────────────

let S: State | null = null;
let profiles: Profile[] = [];
let view: View = "dashboard";
let selectedGame = "";
let lastLearn: LearnView | null = null;
let gateForced = false;
let authMode: "login" | "register" = "login";
let urlDirty = false;
let serverState: "untested" | "ok" | "fail" = "untested";
/** per-host review state, kept across re-renders: key = game|proto|ip */
const sel = new Map<string, { on: boolean; mode: "host" | "prefix" }>();
const selKey = (e: LearnEntry) => `${selectedGame}|${e.proto}|${e.ip}`;
const portList = (e: LearnEntry) => Object.keys(e.ports).map(Number).sort((a, b) => a - b);

const profileFor = (exe: string) => profiles.find((p) => p.game === gameKey(exe));
const isRunning = (s: State, exe: string) => s.procs.some((p) => (p.game ?? p.name.toLowerCase()) === gameKey(exe));
const gameName = (s: State, exe: string) => s.meta?.[exe]?.name ?? gameKey(exe).replace(/\.exe$/, "");

// ───────────────────────── navigation ─────────────────────────

function setView(v: View) {
  view = v;
  store.set("gpn.view", v);
  document.querySelectorAll<HTMLElement>(".view").forEach((el) => el.classList.toggle("active", el.id === `view-${v}`));
  document.querySelectorAll<HTMLElement>("[data-nav]").forEach((b) => {
    if (b.dataset.nav === v) b.setAttribute("aria-current", "page");
    else b.removeAttribute("aria-current");
  });
  $("main").scrollTo({ top: 0 });
  if (v === "learn") refreshLearn();
  if (v === "profiles") refreshProfiles();
}

// ───────────────────────── rendering ─────────────────────────

function render(s: State) {
  S = s;
  renderChrome(s);
  renderGate(s);
  renderDashboard(s);
  renderGames(s);
  renderLearnHeader(s);
  renderSettings(s);
}

function renderChrome(s: State) {
  $("accountName").textContent = s.auth?.username ?? (s.hasToken ? "Machine token" : "Not signed in");
  $("accountRole").textContent = s.auth ? s.auth.role : s.hasToken ? "headless" : "Sign in…";
  $("accountAvatar").textContent = s.auth ? s.auth.username.charAt(0).toUpperCase() : "?";
  const running = s.watched.filter((g) => isRunning(s, g)).length;
  const pip = $("navGamesPip");
  pip.hidden = running === 0;
  pip.textContent = String(running);
  $("navLearnPip").hidden = !s.learning;

  // status bar
  const h = heroModel(s);
  $("sbDot").className = "dot " + h.state;
  $("sbConn").textContent = h.title + (s.tunnel.tunnelUp ? ` · ${plural(s.tunnel.allowedIps.length, "server")}` : "");
  const live = s.watched.find((g) => isRunning(s, g));
  $("sbGame").textContent = live ? `${gameName(s, live)} running` : "";
  $("sbCapture").textContent = s.etw.state === "running" ? "Capture on" : s.etw.state === "denied" ? "Capture needs admin" : "Capture off";
  const host = s.gpnServerUrl.replace(/^https?:\/\//, "").replace(/\/.*$/, "");
  $("sbServer").textContent = (s.auth ? `${s.auth.username} @ ` : "") + host;
}

function heroModel(s: State): { state: "ok" | "busy" | "err" | "warn" | "idle"; icon: string; title: string; text: string; cta?: { label: string; icon: string; act: string } } {
  const t = s.tunnel;
  const running = s.watched.filter((g) => isRunning(s, g));
  if (!s.auth && !s.hasToken)
    return { state: "warn", icon: "shield-alert", title: "Not signed in", text: "Sign in to link this PC to your GPN server.", cta: { label: "Sign in", icon: "login", act: "signin" } };
  if (t.lastError) {
    const admin = /administrator|elevat|access is denied/i.test(t.lastError);
    return { state: "err", icon: "shield-alert", title: "Tunnel problem", text: t.lastError, cta: admin ? { label: "Relaunch as admin", icon: "shield", act: "admin" } : undefined };
  }
  if (t.tunnelUp)
    return { state: "ok", icon: "shield-check", title: "Protected", text: `Routing ${plural(t.allowedIps.length, "server")} through GPN${running[0] ? ` for ${gameName(s, running[0])}` : ""}.` };
  if (running.length)
    return { state: "busy", icon: "plug", title: "Connecting…", text: `Detecting ${gameName(s, running[0]!)}'s servers and bringing the tunnel up.` };
  if (!s.watched.length)
    return { state: "idle", icon: "gamepad", title: "Add your first game", text: "Tell GPN which game to watch and it routes its traffic automatically.", cta: { label: "Add game…", icon: "plus", act: "add-game" } };
  const hasProfile = s.watched.some((g) => profileFor(g));
  return {
    state: "idle",
    icon: "shield",
    title: "Ready",
    text: `Launch ${gameName(s, s.watched[0]!)}${s.watched.length > 1 ? " (or any watched game)" : ""} and it is routed automatically.${hasProfile ? "" : " Tip: record a profile in Learn so routing is in place before the game connects."}`,
  };
}

function renderDashboard(s: State) {
  const h = heroModel(s);
  $("hero").dataset.state = h.state;
  $("heroIcon").innerHTML = ICONS[h.icon] ?? "";
  $("heroTitle").textContent = h.title;
  $("heroText").textContent = h.text;
  $("heroCta").innerHTML = h.cta ? `<button class="btn accent" data-act="${h.cta.act}">${icon(h.cta.icon)}${esc(h.cta.label)}</button>` : "";

  const t = s.tunnel;
  const etw = { running: "On", denied: "Needs admin", error: "Error", off: "Off", unsupported: "N/A" }[s.etw.state];
  $("stats").innerHTML = (
    [
      ["Servers routed", String(t.allowedIps.length), t.tunnelUp ? "through the tunnel" : "tunnel is down", false],
      ["Handshake", t.tunnelUp ? (t.handshakeAgeSecs === null ? "waiting" : `${t.handshakeAgeSecs}s ago`) : "—", "last key exchange", false],
      ["VPN address", t.vpnIp ?? "—", t.registered ? "assigned by server" : "not registered", true],
      ["Traffic capture", etw, "UDP + TCP detection", false],
    ] as const
  )
    .map(([k, v, sub, mono]) => `<div class="stat"><div class="k">${k}</div><div class="v ${mono ? "mono sel" : ""}">${esc(v)}</div><div class="s">${esc(sub)}</div></div>`)
    .join("");

  const active = s.watched.filter((g) => isRunning(s, g));
  $("dashGamesCount").textContent = active.length ? String(active.length) : "";
  $("dashGames").innerHTML = active.length
    ? active.map((g) => gameCard(s, g, false)).join("")
    : `<div class="card"><div class="empty"><div class="ring">${icon("gamepad")}</div><b>No game running</b><p>Start a watched game and it appears here with its live servers.</p></div></div>`;

  $("serverCount").textContent = s.targets.length ? String(s.targets.length) : "";
  const routed = new Set(t.allowedIps);
  $("serverRows").innerHTML = s.targets.length
    ? s.targets
        .map(
          (x) => `<tr>
            <td class="mono">${esc(x.ip)}</td>
            <td><span class="chip ${esc(x.proto)}">${esc(x.proto.toUpperCase())}</span></td>
            <td class="mono">${x.port ?? "—"}</td>
            <td>${routed.has(x.ip) ? '<span class="chip dot ok">via GPN</span>' : '<span class="chip">direct</span>'}</td>
          </tr>`
        )
        .join("")
    : `<tr><td colspan="4"><div class="empty"><div class="ring">${icon("radar")}</div><b>No game traffic yet</b><p>Servers appear here the moment a watched game connects.</p></div></td></tr>`;
}

function gameCard(s: State, exe: string, actions: boolean): string {
  const m = s.meta?.[exe] ?? null;
  const name = gameName(s, exe);
  const running = isRunning(s, exe);
  const prof = profileFor(exe);
  const iconHtml = m?.icon ? `<img class="gicon" src="${m.icon}" alt="" draggable="false">` : `<div class="gicon fallback">${esc(name.charAt(0).toUpperCase())}</div>`;
  const chips = [
    running ? '<span class="chip dot ok live">Running</span>' : '<span class="chip">Not running</span>',
    prof ? `<span class="chip tcp">Profile · ${plural(prof.entries.length, "server")}</span>` : '<span class="chip">No profile</span>',
    m?.company ? `<span class="chip">${esc(m.company)}</span>` : "",
  ].join("");
  const buttons = actions
    ? `<button class="btn sm" data-learn="${esc(exe)}">${icon("crosshair")}Learn</button>
       <button class="btn sm icon subtle danger-text" data-remove-game="${esc(exe)}" title="Remove" aria-label="Remove ${esc(name)}">${icon("trash")}</button>`
    : "";
  return `<div class="game">
    ${iconHtml}
    <div class="grow">
      <div class="name">${esc(name)}</div>
      <div class="path sel" title="${esc(exe)}">${esc(exe)}</div>
      <div class="chips">${chips}</div>
    </div>
    <div class="btns">${buttons}</div>
  </div>`;
}

function renderGames(s: State) {
  $("gamesCount").textContent = s.watched.length ? String(s.watched.length) : "";
  $("gameList").innerHTML = s.watched.length
    ? s.watched.map((g) => gameCard(s, g, true)).join("")
    : `<div class="card"><div class="empty"><div class="ring">${icon("gamepad")}</div><b>Nothing watched yet</b><p>Add a game to start. GPN reads its name and icon from the file.</p></div></div>`;
}

// ───────────────────────── learn ─────────────────────────

function renderLearnHeader(s: State) {
  const toggle = $("learnToggle");
  toggle.className = s.learning ? "btn" : "btn rec";
  toggle.innerHTML = s.learning ? `${icon("x")}Stop` : `${icon("radar")}Start recording`;
  const rec = $("recChip");
  rec.className = "chip dot " + (s.learning ? "err live" : "");
  rec.textContent = s.learning ? "Recording" : "Idle";

  const select = $<HTMLSelectElement>("learnGame");
  const sig = s.watched.join("|") + "#" + s.watched.map((g) => gameName(s, g)).join("|");
  if (select.dataset.sig !== sig) {
    select.dataset.sig = sig;
    select.innerHTML = s.watched.map((exe) => `<option value="${esc(exe)}">${esc(gameName(s, exe))}</option>`).join("");
  }
  if (!s.watched.includes(selectedGame)) selectedGame = s.watched[0] ?? "";
  select.value = selectedGame;
  select.disabled = !s.watched.length;
  $<HTMLButtonElement>("learnToggle").disabled = !s.watched.length;

  let banner = "";
  if (!s.watched.length) {
    banner = `<div class="banner warn"><div class="grow"><b>Add a game first.</b> Learn mode records the servers of a watched game.</div><button class="btn sm" data-go="games">Go to Games</button></div>`;
  } else if (s.etw.state === "denied") {
    banner = `<div class="banner err"><div class="grow"><b>Capture needs administrator rights.</b> Without it UDP servers can't be seen.</div><button class="btn sm" data-act="admin">Relaunch as admin</button></div>`;
  } else if (s.etw.state === "error" || s.etw.state === "off") {
    banner = `<div class="banner warn"><div class="grow"><b>Traffic capture isn't running.</b> ${esc(s.etw.message)}</div><button class="btn sm" data-act="etw-retry">Retry</button></div>`;
  } else if (s.etw.state === "unsupported") {
    banner = `<div class="banner warn"><div class="grow"><b>Capture unavailable.</b> ${esc(s.etw.message)}</div></div>`;
  }
  $("learnBanner").innerHTML = banner;
}

function renderLearnRows() {
  const tb = $("learnRows");
  const entries = lastLearn?.entries ?? [];
  $("mSessions").textContent = String(lastLearn?.sessions ?? 0);
  $("mHosts").textContent = String(entries.length);
  $("reviewInfo").textContent = entries.length ? String(entries.length) : "";

  if (!selectedGame || !entries.length) {
    const rec = S?.learning;
    tb.innerHTML = `<tr><td colspan="6"><div class="empty"><div class="ring">${icon("radar")}</div>
      <b>${rec ? "Listening…" : "Nothing recorded yet"}</b>
      <p>${rec ? "Launch the game and play. Servers appear here as they're seen." : "Press “Start recording”, then launch and play the game."}</p></div></td></tr>`;
    updateSaveBar();
    return;
  }

  const max = Math.max(...entries.map((e) => e.total_bytes), 1);
  tb.innerHTML = entries
    .map((e) => {
      const k = selKey(e);
      if (!sel.has(k)) sel.set(k, { on: e.suggested, mode: "host" });
      const st = sel.get(k)!;
      const ports = portList(e);
      const portTxt = ports.length > 3 ? `${ports.slice(0, 3).join(", ")} +${ports.length - 3}` : ports.join(", ");
      const prefixOk = !!e.asn && e.asn.prefix !== "NA" && prefixLen(e.asn.prefix) >= 16;
      const cloud = !!e.asn && CLOUD_RE.test(e.asn.name);
      const net = e.asn
        ? `<div>${esc(e.asn.name)}</div><div class="sub mono">AS${e.asn.asn} · ${esc(e.asn.prefix)} ${cloud ? '<span class="chip warn">shared cloud</span>' : ""}</div>`
        : '<span class="faint">looking up…</span>';
      return `<tr class="${st.on ? "picked" : ""}">
        <td><input type="checkbox" class="sel-on" data-k="${esc(k)}" ${st.on ? "checked" : ""} aria-label="Include ${esc(e.ip)}"></td>
        <td><div class="mono">${esc(e.ip)}</div><div class="sub"><span class="chip ${esc(e.proto)}">${esc(e.proto.toUpperCase())}</span> <span class="mono">${esc(portTxt)}</span></div></td>
        <td class="num">${fmtBytes(e.total_bytes)}<div class="bar"><i style="width:${Math.max(3, (e.total_bytes / max) * 100)}%"></i></div></td>
        <td class="num">${e.sessions}</td>
        <td>${net}</td>
        <td><div class="seg">
          <button data-k="${esc(k)}" data-mode="host" aria-pressed="${st.mode === "host"}">Host</button>
          <button data-k="${esc(k)}" data-mode="prefix" aria-pressed="${st.mode === "prefix"}" ${prefixOk ? "" : "disabled"} title="${prefixOk ? "Route " + esc(e.asn!.prefix) : "No usable prefix"}">${prefixOk ? esc(e.asn!.prefix) : "Prefix"}</button>
        </div></td>
      </tr>`;
    })
    .join("");
  updateSaveBar();
}

function updateSaveBar() {
  const entries = lastLearn?.entries ?? [];
  const n = buildEntries().length;
  $("saveBar").style.display = entries.length ? "flex" : "none";
  $("saveCount").textContent = plural(n, "server") + " selected";
  const existing = selectedGame ? profileFor(selectedGame) : undefined;
  $("saveHint").textContent = existing
    ? `Saving replaces the current profile (${plural(existing.entries.length, "server")}).`
    : "Tick the servers that belong to the game.";
  $<HTMLButtonElement>("saveProfileBtn").disabled = n === 0;
}

async function refreshLearn() {
  if (!selectedGame) {
    lastLearn = null;
    renderLearnRows();
    return;
  }
  try {
    lastLearn = await invoke<LearnView>("get_learn", { game: selectedGame });
    // don't yank an open control out from under the user
    if (view !== "learn" || !$("learnRows").contains(document.activeElement)) renderLearnRows();
  } catch (e) {
    console.error(e);
  }
}

function buildEntries(): ProfileEntry[] {
  const out: ProfileEntry[] = [];
  for (const e of lastLearn?.entries ?? []) {
    const st = sel.get(selKey(e));
    if (!st?.on) continue;
    const base = { proto: e.proto, asn: e.asn?.asn ?? null, as_name: e.asn?.name ?? null, note: "" };
    if (st.mode === "prefix" && e.asn) {
      out.push({ ...base, cidr: e.asn.prefix, ports: [], source: "prefix" });
    } else {
      const ports = portList(e);
      out.push({ ...base, cidr: `${e.ip}/32`, ports: ports.length <= 8 ? ports : [], source: "observed" });
    }
  }
  return out;
}

// ───────────────────────── profiles ─────────────────────────

async function refreshProfiles() {
  try {
    profiles = await invoke<Profile[]>("get_profiles");
  } catch (e) {
    console.error(e);
    return;
  }
  renderProfiles();
  if (S) {
    renderGames(S);
    renderDashboard(S);
  }
}

function renderProfiles() {
  const list = $("profileList");
  if (!profiles.length) {
    list.innerHTML = `<div class="card"><div class="empty"><div class="ring">${icon("folder")}</div><b>No profiles yet</b>
      <p>Record a game in Learn, tick its servers and save them. They're routed the moment the game starts.</p>
      <button class="btn accent" data-go="learn">${icon("crosshair")}Open Learn</button></div></div>`;
    return;
  }
  list.innerHTML = profiles
    .map((p) => {
      const shown = p.entries.slice(0, 14);
      return `<div class="card profile">
        <div class="head">
          <div class="grow"><div style="font-weight:600;font-size:14px">${esc(p.name)}</div>
            <div class="dim small">${esc(p.game)} · ${plural(p.entries.length, "entry", "entries")} · updated ${esc(new Date(p.updated).toLocaleDateString())}</div></div>
          <button class="btn sm" data-export="${esc(p.game)}">${icon("download")}Export</button>
          <button class="btn sm icon subtle danger-text" data-remove-profile="${esc(p.game)}" title="Delete" aria-label="Delete profile">${icon("trash")}</button>
        </div>
        <div class="cidrs">${shown
          .map((e) => `<span class="cidr">${esc(e.cidr)}<i>${esc(e.proto)}${e.ports.length ? ":" + e.ports.join(",") : ""}</i></span>`)
          .join("")}${p.entries.length > shown.length ? `<span class="cidr"><i>+${p.entries.length - shown.length} more</i></span>` : ""}</div>
      </div>`;
    })
    .join("");
}

// ───────────────────────── settings & auth ─────────────────────────

function renderSettings(s: State) {
  const url = $<HTMLInputElement>("serverUrl");
  if (!urlDirty && document.activeElement !== url) url.value = s.gpnServerUrl || "";
  $("insecureWarn").style.display = s.insecureUrl ? "" : "none";
  const chip = $("serverChip");
  chip.className = "chip dot " + (serverState === "ok" ? "ok" : serverState === "fail" ? "err" : "");
  chip.textContent = serverState === "ok" ? "Reachable" : serverState === "fail" ? "Unreachable" : "Not tested";

  $("accountCard").innerHTML = s.auth
    ? `<span class="avatar">${esc(s.auth.username.charAt(0).toUpperCase())}</span>
       <div class="info"><b>${esc(s.auth.username)}</b><div class="d">${esc(s.auth.role)}</div></div>
       <button class="btn" data-act="logout">${icon("logout")}Sign out</button>`
    : `<div class="info"><b>${s.hasToken ? "Using the machine token" : "Not signed in"}</b><div class="d">${s.hasToken ? "Sign in to use your personal account." : "Sign in to route your games."}</div></div>
       <button class="btn accent" data-act="signin">${icon("login")}Sign in</button>`;
}

function renderGate(s: State) {
  const show = (!s.auth && !s.hasToken) || gateForced;
  $("gate").classList.toggle("show", show);
  const urlInput = $<HTMLInputElement>("gateServerUrl");
  if (show && document.activeElement !== urlInput && !urlInput.dataset.touched) urlInput.value = s.gpnServerUrl;
  $("gateServerLabel").textContent = (urlInput.value || s.gpnServerUrl).replace(/^https?:\/\//, "");
  if (show && !$("gate").dataset.focused) {
    $("gate").dataset.focused = "1";
    setTimeout(() => $<HTMLInputElement>("authUser").focus(), 50);
  }
  if (!show) delete $("gate").dataset.focused;
}

function setAuthMode(m: "login" | "register") {
  authMode = m;
  $("tabLogin").setAttribute("aria-selected", String(m === "login"));
  $("tabRegister").setAttribute("aria-selected", String(m === "register"));
  $("inviteField").style.display = m === "register" ? "" : "none";
  $("gateTitle").textContent = m === "login" ? "Welcome back" : "Create your account";
  $("gateLead").textContent = m === "login" ? "Sign in to route your games through your server." : "Registers a new account on your GPN server.";
  $("authSubmit").textContent = m === "login" ? "Sign in" : "Create account";
  $("authMsg").textContent = "";
  $<HTMLInputElement>("authPass").autocomplete = m === "login" ? "current-password" : "new-password";
}

// ───────────────────────── actions ─────────────────────────

async function addGame(exe: string) {
  exe = exe.trim().replace(/^"(.*)"$/, "$1");
  if (!exe) return;
  if (!/\.exe$/i.test(exe)) return toast("That isn't an .exe file", "err");
  try {
    await invoke("watch_game", { exe });
    toast(`Added ${gameKey(exe)}`, "ok");
    setView("games");
    refresh();
  } catch (e) {
    toast(errText(e), "err");
  }
}

async function browseForGame() {
  try {
    const picked = await openDialog({
      multiple: false,
      directory: false,
      title: "Select the game's executable",
      filters: [{ name: "Programs", extensions: ["exe"] }],
    });
    if (typeof picked === "string") await addGame(picked);
  } catch (e) {
    toast(errText(e), "err");
  }
}

async function doAction(act: string) {
  switch (act) {
    case "signin":
      gateForced = true;
      if (S) renderGate(S);
      break;
    case "add-game":
      setView("games");
      await browseForGame();
      break;
    case "admin":
      try {
        await invoke("relaunch_as_admin");
        toast("Restarting with administrator rights…");
      } catch (e) {
        toast(errText(e), "err");
      }
      break;
    case "etw-retry":
      await invoke("etw_restart");
      refresh();
      break;
    case "logout":
      await invoke("auth_logout");
      gateForced = false;
      refresh();
      break;
  }
}

async function refresh() {
  try {
    render(await invoke<State>("get_state"));
  } catch (e) {
    console.error(e);
  }
}

// ───────────────────────── native-app behaviour ─────────────────────────

function nativeFeel() {
  // never navigate or open a dropped file inside the webview
  window.addEventListener("dragover", (e) => e.preventDefault());
  window.addEventListener("drop", (e) => e.preventDefault());
  window.addEventListener("dragstart", (e) => e.preventDefault());

  if (!import.meta.env.DEV) {
    // production builds shouldn't expose browser UI: context menu, reload, zoom, print, devtools keys
    window.addEventListener("contextmenu", (e) => {
      if (!(e.target as HTMLElement).closest("input, textarea")) e.preventDefault();
    });
    window.addEventListener("keydown", (e) => {
      const k = e.key.toLowerCase();
      const blocked =
        e.key === "F5" || e.key === "F7" || e.key === "F12" ||
        (e.ctrlKey && ["r", "p", "u", "s", "g", "+", "-", "=", "0"].includes(k)) ||
        (e.ctrlKey && e.shiftKey && ["i", "j", "c", "r"].includes(k));
      if (blocked) e.preventDefault();
    });
    window.addEventListener("wheel", (e) => { if (e.ctrlKey) e.preventDefault(); }, { passive: false });
  }

  // keyboard shortcuts: Ctrl+1…5 switch pages, Ctrl+, opens settings, Esc closes the sign-in sheet
  document.addEventListener("keydown", (e) => {
    if (e.ctrlKey && !e.shiftKey && !e.altKey) {
      const n = parseInt(e.key, 10);
      if (n >= 1 && n <= VIEWS.length) { e.preventDefault(); setView(VIEWS[n - 1]!); }
      else if (e.key === ",") { e.preventDefault(); setView("settings"); }
    }
    if (e.key === "Escape" && gateForced) {
      gateForced = false;
      if (S) renderGate(S);
    }
  });
  document.querySelectorAll<HTMLElement>("[data-nav]").forEach((b) => {
    const i = VIEWS.indexOf(b.dataset.nav as View);
    b.title = `${b.querySelector(".label")?.textContent ?? ""} (Ctrl+${i + 1})`;
  });
}

async function windowChrome() {
  const win = getCurrentWindow();
  $("winMin").addEventListener("click", () => void win.minimize());
  $("winClose").addEventListener("click", () => void win.close());

  // drag an .exe onto the window to watch it
  const veil = $("dropVeil");
  await getCurrentWebview()
    .onDragDropEvent((ev) => {
      const p = ev.payload;
      if (p.type === "enter" || p.type === "over") veil.classList.add("show");
      else if (p.type === "leave") veil.classList.remove("show");
      else if (p.type === "drop") {
        veil.classList.remove("show");
        const exes = p.paths.filter((x) => /\.exe$/i.test(x));
        if (!exes.length) toast("Drop an .exe file", "err");
        exes.forEach((x) => void addGame(x));
      }
    })
    .catch(() => {});
}

// ───────────────────────── wiring ─────────────────────────

function wire() {
  // delegated clicks
  document.addEventListener("click", async (ev) => {
    const t = (ev.target as HTMLElement).closest<HTMLElement>("[data-nav],[data-go],[data-act],[data-learn],[data-remove-game],[data-remove-profile],[data-export],[data-theme-pick]");
    if (!t) return;
    if (t.dataset.themePick) setThemePref(t.dataset.themePick as ThemePref);
    else if (t.dataset.nav) setView(t.dataset.nav as View);
    else if (t.dataset.go) setView(t.dataset.go as View);
    else if (t.dataset.act) void doAction(t.dataset.act);
    else if (t.dataset.learn) {
      selectedGame = t.dataset.learn;
      if (S) renderLearnHeader(S);
      setView("learn");
    } else if (t.dataset.removeGame) {
      const exe = t.dataset.removeGame;
      const name = S ? gameName(S, exe) : exe;
      if (await confirmDialog({ title: `Remove ${name}?`, body: "GPN stops watching this game. Its saved profile and learned servers are kept.", ok: "Remove", danger: true })) {
        await invoke("unwatch_game", { exe });
        toast(`Removed ${name}`);
        refresh();
      }
    } else if (t.dataset.removeProfile) {
      const game = t.dataset.removeProfile;
      const p = profiles.find((x) => x.game === game);
      if (await confirmDialog({ title: `Delete the profile for ${p?.name ?? game}?`, body: "Routing for this game falls back to live detection only.", ok: "Delete", danger: true })) {
        await invoke("delete_profile", { game });
        toast("Profile deleted");
        refreshProfiles();
      }
    } else if (t.dataset.export) {
      try {
        const path = await invoke<string>("export_profile", { game: t.dataset.export });
        toast(`Exported to ${path}`, "ok");
      } catch (e) {
        toast(errText(e), "err");
      }
    }
  });

  $("accountChip").addEventListener("click", () => (S?.auth || S?.hasToken ? setView("settings") : void doAction("signin")));

  // games
  $("browseBtn").addEventListener("click", () => void browseForGame());
  $("watchBtn").addEventListener("click", () => {
    const inp = $<HTMLInputElement>("exeInput");
    void addGame(inp.value).then(() => (inp.value = ""));
  });
  $("exeInput").addEventListener("keydown", (e) => (e as KeyboardEvent).key === "Enter" && $("watchBtn").click());

  // learn
  $("learnGame").addEventListener("change", () => {
    selectedGame = $<HTMLSelectElement>("learnGame").value;
    void refreshLearn();
  });
  $("learnToggle").addEventListener("click", async () => {
    await invoke("set_learning", { enabled: !S?.learning });
    await refresh();
    toast(S?.learning ? "Recording — launch the game" : "Recording stopped");
  });
  $("learnClear").addEventListener("click", async () => {
    if (!selectedGame) return;
    if (!(await confirmDialog({ title: "Clear recorded servers?", body: "This forgets everything learned for this game so far.", ok: "Clear", danger: true }))) return;
    await invoke("clear_learn", { game: selectedGame });
    for (const k of [...sel.keys()]) if (k.startsWith(selectedGame + "|")) sel.delete(k);
    void refreshLearn();
  });
  $("learnRows").addEventListener("change", (ev) => {
    const t = ev.target as HTMLInputElement;
    if (!t.classList.contains("sel-on") || !t.dataset.k) return;
    const st = sel.get(t.dataset.k) ?? { on: false, mode: "host" as const };
    st.on = t.checked;
    sel.set(t.dataset.k, st);
    renderLearnRows();
  });
  $("learnRows").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest<HTMLButtonElement>("button[data-mode]");
    if (!b?.dataset.k || b.disabled) return;
    const st = sel.get(b.dataset.k) ?? { on: true, mode: "host" as const };
    st.mode = b.dataset.mode as "host" | "prefix";
    st.on = true;
    sel.set(b.dataset.k, st);
    renderLearnRows();
  });
  $("saveProfileBtn").addEventListener("click", async () => {
    const entries = buildEntries();
    if (!entries.length) return;
    try {
      const p = await invoke<Profile>("save_profile", { game: selectedGame, name: null, entries });
      toast(`Profile saved · ${plural(p.entries.length, "entry", "entries")}`, "ok");
      await refreshProfiles();
      updateSaveBar();
    } catch (e) {
      toast(errText(e), "err");
    }
  });

  // settings
  $("serverUrl").addEventListener("input", () => (urlDirty = true));
  $("saveBtn").addEventListener("click", async () => {
    const token = $<HTMLInputElement>("token").value.trim();
    try {
      await invoke("save_settings", { serverUrl: $<HTMLInputElement>("serverUrl").value.trim(), token: token || null });
      $<HTMLInputElement>("token").value = "";
      urlDirty = false;
      serverState = "untested";
      toast("Settings saved", "ok");
      void refresh();
    } catch (e) {
      toast(errText(e), "err");
    }
  });
  $("pingBtn").addEventListener("click", async () => {
    $("serverChip").className = "chip dot";
    $("serverChip").textContent = "Testing…";
    try {
      await invoke("ping_server");
      serverState = "ok";
    } catch (e) {
      serverState = "fail";
      toast(errText(e), "err");
    }
    if (S) renderSettings(S);
  });

  // sign-in
  $("tabLogin").addEventListener("click", () => setAuthMode("login"));
  $("tabRegister").addEventListener("click", () => setAuthMode("register"));
  $("gateServerUrl").addEventListener("input", () => ($("gateServerUrl").dataset.touched = "1"));
  $("gateForm").addEventListener("submit", async (e) => {
    e.preventDefault();
    const msg = $("authMsg");
    const submit = $<HTMLButtonElement>("authSubmit");
    msg.textContent = "";
    const username = $<HTMLInputElement>("authUser").value.trim();
    const password = $<HTMLInputElement>("authPass").value;
    if (!username || !password) return void (msg.textContent = "Enter a username and password.");
    submit.disabled = true;
    submit.textContent = authMode === "login" ? "Signing in…" : "Creating…";
    try {
      const url = $<HTMLInputElement>("gateServerUrl").value.trim();
      if (url && S && url !== S.gpnServerUrl) await invoke("save_settings", { serverUrl: url, token: null });
      if (authMode === "login") await invoke("auth_login", { username, password });
      else await invoke("auth_register", { username, password, inviteCode: $<HTMLInputElement>("authInvite").value.trim() || null });
      $<HTMLInputElement>("authPass").value = "";
      gateForced = false;
      delete $("gateServerUrl").dataset.touched;
      toast(`Signed in as ${username}`, "ok");
      await refresh();
    } catch (err) {
      msg.textContent = errText(err);
    } finally {
      submit.disabled = false;
      submit.textContent = authMode === "login" ? "Sign in" : "Create account";
    }
  });
}

async function boot() {
  // In a plain browser (no Tauri runtime) `bun run dev` serves a mocked backend for UI work.
  if (import.meta.env.DEV && !(window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__) {
    await import("./dev-mock");
  }
  paintIcons();
  applyTheme();
  renderThemePicker();
  nativeFeel();
  const saved = store.get("gpn.view") as View | null;
  setView(saved && VIEWS.includes(saved) ? saved : "dashboard");
  setAuthMode("login");
  wire();
  void windowChrome();

  void listen<State>("gpn-state", (ev) => render(ev.payload));
  void refresh();
  void refreshLearn();
  void refreshProfiles();
  setInterval(refresh, 3000);
  setInterval(refreshLearn, 3000);
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", () => void boot());
else void boot();
