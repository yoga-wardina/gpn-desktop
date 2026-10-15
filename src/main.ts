import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface GameProc { pid: number; name: string }
interface Target { ip: string; port?: number; proto: string }
interface GameMeta { name: string; company?: string; icon?: string; source: string }
interface EtwStatus { state: "off" | "running" | "denied" | "error" | "unsupported"; message: string }
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
interface TunnelView {
  registered: boolean;
  vpnIp: string | null;
  tunnelUp: boolean;
  allowedIps: string[];
  handshakeAgeSecs: number | null;
  lastError: string | null;
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

function esc(s: unknown): string {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c] as string)
  );
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const gameKey = (exe: string) => (exe.split(/[\\/]/).pop() || exe).toLowerCase();

function render(s: State) {
  const dot = $("dot");
  dot.classList.remove("off");
  $("statusText").textContent = "monitoring";
  $<HTMLInputElement>("serverUrl").value = s.gpnServerUrl || "";

  const games = $("games");
  games.innerHTML = s.watched.length
    ? ""
    : '<div class="empty">No games watched. Add an executable above.</div>';
  for (const exe of s.watched) {
    const live = s.procs.some((p) => p.name.toLowerCase().includes(gameKey(exe)));
    const m = s.meta?.[exe] || null;
    const displayName = m ? m.name : exe;
    const iconHtml = m?.icon
      ? `<img src="${m.icon}" class="gicon" alt="">`
      : `<div class="icon">${esc(displayName.charAt(0).toUpperCase())}</div>`;
    const subBits: string[] = [];
    if (live) subBits.push('<span style="color:var(--green)">● running</span>');
    if (m?.company) subBits.push(esc(m.company));
    if (m?.source === "versioninfo") subBits.push('<span style="opacity:.6">verified from exe</span>');
    games.insertAdjacentHTML(
      "beforeend",
      `<div class="card">
        ${iconHtml}
        <div>
          <div class="name">${esc(displayName)}</div>
          <div class="sub">${subBits.join(" · ") || "not running"}</div>
          <div class="sub mono" style="opacity:.6">${esc(exe)}</div>
        </div>
        <button data-exe="${esc(exe)}" class="remove-btn">Remove</button>
      </div>`
    );
  }
  games.querySelectorAll(".remove-btn").forEach((b) =>
    b.addEventListener("click", () => invoke("unwatch_game", { exe: (b as HTMLElement).dataset.exe }))
  );

  const tb = $("targets");
  if (!s.targets.length) {
    tb.innerHTML = '<tr><td colspan="3" class="empty">No game traffic detected yet.</td></tr>';
  } else {
    tb.innerHTML = s.targets
      .map(
        (t) => `<tr>
          <td><span class="tag ${esc(t.proto)}">${esc(t.proto.toUpperCase())}</span></td>
          <td class="mono">${esc(t.ip)}</td>
          <td class="mono">${t.port ?? "—"}</td>
        </tr>`
      )
      .join("");
  }

  renderLearnBar(s);
  renderAccount(s);
}

function renderAccount(s: State) {
  $("insecureWarn").style.display = s.insecureUrl ? "" : "none";
  $("loggedOut").style.display = s.auth ? "none" : "";
  $("loggedIn").style.display = s.auth ? "" : "none";
  if (s.auth) {
    $("authName").textContent = s.auth.username;
    $("authRole").textContent = s.auth.role;
  }
  const t = s.tunnel;
  const tag = $("tunnelTag");
  tag.className = "tag " + (t.tunnelUp ? "tcp" : "udp");
  tag.textContent = t.tunnelUp ? "tunnel up" : "tunnel down";
  const bits: string[] = [];
  if (t.vpnIp) bits.push(`VPN IP ${t.vpnIp}`);
  if (t.tunnelUp) bits.push(t.handshakeAgeSecs === null ? "no handshake yet" : `handshake ${t.handshakeAgeSecs}s ago`);
  if (!s.auth && !s.hasToken) bits.push("log in to use the tunnel");
  $("tunnelInfo").textContent = bits.join(" · ");
  $("tunnelRoutes").innerHTML = t.allowedIps.length
    ? "routing: " + t.allowedIps.slice(0, 12).map(esc).join(", ") + (t.allowedIps.length > 12 ? ` … +${t.allowedIps.length - 12}` : "")
    : "";
  $("tunnelErr").textContent = t.lastError ?? "";
}

// ===== learn mode =====

let learning = false;
let selectedGame = "";
let lastLearn: LearnView | null = null;
/** per-host review state, kept across re-renders: key = game|proto|ip */
const sel = new Map<string, { on: boolean; mode: "host" | "prefix" }>();

const CLOUD_RE = /amazon|google|microsoft|azure|cloudflare|akamai|fastly|oracle|digitalocean|ovh|hetzner|linode|alibaba|tencent|vultr|cdn/i;
const prefixLen = (p: string) => parseInt(p.split("/")[1] ?? "32", 10);
const fmtBytes = (b: number) =>
  b >= 1048576 ? (b / 1048576).toFixed(1) + " MB" : b >= 1024 ? (b / 1024).toFixed(1) + " KB" : b + " B";
const selKey = (e: LearnEntry) => `${selectedGame}|${e.proto}|${e.ip}`;
const portList = (e: LearnEntry) => Object.keys(e.ports).map(Number).sort((a, b) => a - b);

function renderLearnBar(s: State) {
  learning = s.learning;
  const tag = $("etwTag");
  tag.className = "tag " + (s.etw.state === "running" ? "tcp" : "udp");
  tag.textContent = "ETW · " + s.etw.state;
  $("etwMsg").textContent = s.etw.message;
  $("etwAdminBtn").style.display = s.etw.state === "denied" ? "" : "none";
  $("etwRetryBtn").style.display = s.etw.state === "error" || s.etw.state === "off" ? "" : "none";
  $("learnToggle").textContent = s.learning ? "Stop learning" : "Start learning";

  // rebuild the game <select> only when the watched list changes (keeps the open dropdown stable)
  const select = $<HTMLSelectElement>("learnGame");
  const sig = s.watched.join("|");
  if (select.dataset.sig !== sig) {
    select.dataset.sig = sig;
    select.innerHTML = s.watched
      .map((exe) => `<option value="${esc(exe)}">${esc(s.meta?.[exe]?.name ?? gameKey(exe))}</option>`)
      .join("");
  }
  if (!s.watched.includes(selectedGame)) selectedGame = s.watched[0] ?? "";
  select.value = selectedGame;
}

function renderLearnRows() {
  const tb = $("learnRows");
  if (!lastLearn || !selectedGame) {
    tb.innerHTML = '<tr><td colspan="8" class="empty">Watch a game to start learning its servers.</td></tr>';
    return;
  }
  $("learnInfo").textContent = lastLearn.sessions
    ? `${lastLearn.sessions} play session${lastLearn.sessions === 1 ? "" : "s"} · ${lastLearn.entries.length} hosts`
    : "";
  if (!lastLearn.entries.length) {
    tb.innerHTML = `<tr><td colspan="8" class="empty">${
      learning ? "Learning… launch the game and play." : "Nothing learned yet. Press “Start learning”, then play."
    }</td></tr>`;
    return;
  }
  tb.innerHTML = lastLearn.entries
    .map((e) => {
      const k = selKey(e);
      if (!sel.has(k)) sel.set(k, { on: e.suggested, mode: "host" });
      const st = sel.get(k)!;
      const ports = portList(e);
      const portTxt = ports.length > 4 ? `${ports.slice(0, 4).join(", ")} +${ports.length - 4}` : ports.join(", ");
      const prefixOk = !!e.asn && e.asn.prefix !== "NA" && prefixLen(e.asn.prefix) >= 16;
      const cloud = !!e.asn && CLOUD_RE.test(e.asn.name);
      const net = e.asn
        ? `<div>${esc(e.asn.name)}</div><div class="sub mono">AS${e.asn.asn} · ${esc(e.asn.prefix)}${cloud ? ' <span style="color:var(--red)">shared cloud</span>' : ""}</div>`
        : '<span class="sub">looking up…</span>';
      return `<tr>
        <td><input type="checkbox" data-k="${esc(k)}" class="sel-on" ${st.on ? "checked" : ""}></td>
        <td><span class="tag ${esc(e.proto)}">${esc(e.proto.toUpperCase())}</span></td>
        <td class="mono">${esc(e.ip)}</td>
        <td class="mono">${esc(portTxt)}</td>
        <td>${fmtBytes(e.total_bytes)}<div class="sub">↑${fmtBytes(e.bytes_out)} ↓${fmtBytes(e.bytes_in)}</div></td>
        <td>${e.sessions}</td>
        <td>${net}</td>
        <td><select data-k="${esc(k)}" class="sel-mode">
          <option value="host" ${st.mode === "host" ? "selected" : ""}>this host (/32)</option>
          <option value="prefix" ${st.mode === "prefix" ? "selected" : ""} ${prefixOk ? "" : "disabled"}>${
            prefixOk ? "whole " + esc(e.asn!.prefix) : "prefix n/a"
          }</option>
        </select></td>
      </tr>`;
    })
    .join("");
}

async function refreshLearn() {
  if (!selectedGame) {
    lastLearn = null;
    renderLearnRows();
    return;
  }
  try {
    lastLearn = await invoke<LearnView>("get_learn", { game: selectedGame });
    // don't yank an open dropdown / checkbox focus out from under the user
    if (!$("learnRows").contains(document.activeElement)) renderLearnRows();
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

async function renderProfiles() {
  const list = $("profiles");
  const profiles = await invoke<Profile[]>("get_profiles");
  if (!profiles.length) {
    list.innerHTML = '<div class="empty">No saved profiles yet.</div>';
    return;
  }
  list.innerHTML = profiles
    .map(
      (p) => `<div class="card" style="display:block">
        <div class="row" style="align-items:center">
          <div>
            <div class="name">${esc(p.name)} <span class="sub mono">${esc(p.game)}</span></div>
            <div class="sub">${p.entries.length} entr${p.entries.length === 1 ? "y" : "ies"} · updated ${esc(new Date(p.updated).toLocaleString())}</div>
          </div>
          <button class="btn prof-export" data-game="${esc(p.game)}" style="margin-left:auto">Export JSON</button>
          <button class="btn prof-delete" data-game="${esc(p.game)}">Delete</button>
        </div>
        <div class="sub mono" style="margin-top:8px">${p.entries
          .slice(0, 12)
          .map((e) => `${esc(e.cidr)} <span style="opacity:.6">${esc(e.proto)}${e.ports.length ? ":" + e.ports.join(",") : ""}</span>`)
          .join("<br>")}${p.entries.length > 12 ? `<br>… +${p.entries.length - 12} more` : ""}</div>
      </div>`
    )
    .join("");
  list.querySelectorAll<HTMLElement>(".prof-export").forEach((b) =>
    b.addEventListener("click", async () => {
      try {
        const path = await invoke<string>("export_profile", { game: b.dataset.game });
        say("profileMsg", `exported to ${path}`, "var(--green)");
      } catch (e) {
        say("profileMsg", `✗ ${String(e)}`, "var(--red)");
      }
    })
  );
  list.querySelectorAll<HTMLElement>(".prof-delete").forEach((b) =>
    b.addEventListener("click", async () => {
      await invoke("delete_profile", { game: b.dataset.game });
      renderProfiles();
    })
  );
}

function say(id: string, text: string, color = "") {
  const el = $(id);
  el.textContent = text;
  el.style.color = color;
}

async function refresh() {
  try {
    render(await invoke<State>("get_state"));
  } catch (e) {
    console.error(e);
  }
}

window.addEventListener("DOMContentLoaded", () => {
  $("watchBtn").addEventListener("click", async () => {
    const inp = $<HTMLInputElement>("exeInput");
    const exe = inp.value.trim();
    if (!exe) return;
    await invoke("watch_game", { exe });
    inp.value = "";
    refresh();
  });
  $("exeInput").addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") $<HTMLButtonElement>("watchBtn").click();
  });

  $("saveBtn").addEventListener("click", async () => {
    const token = $<HTMLInputElement>("token").value.trim();
    await invoke("save_settings", {
      serverUrl: $<HTMLInputElement>("serverUrl").value.trim(),
      token: token || null, // blank keeps the saved token
    });
    $<HTMLInputElement>("token").value = "";
    say("pingResult", "saved");
    refresh();
  });

  $("pingBtn").addEventListener("click", async () => {
    say("pingResult", "testing…");
    try {
      await invoke("ping_server");
      say("pingResult", "✓ server reachable", "var(--green)");
    } catch (e) {
      say("pingResult", `✗ ${String(e)}`, "var(--red)");
    }
  });

  // ----- account -----
  const authAction = (cmd: "auth_login" | "auth_register") => async () => {
    const username = $<HTMLInputElement>("authUser").value;
    const password = $<HTMLInputElement>("authPass").value;
    const inviteCode = $<HTMLInputElement>("authInvite").value || null;
    say("authMsg", "…");
    try {
      await invoke(cmd, cmd === "auth_register" ? { username, password, inviteCode } : { username, password });
      $<HTMLInputElement>("authPass").value = "";
      say("authMsg", "");
      refresh();
    } catch (e) {
      say("authMsg", `✗ ${String(e)}`, "var(--red)");
    }
  };
  $("loginBtn").addEventListener("click", authAction("auth_login"));
  $("registerBtn").addEventListener("click", authAction("auth_register"));
  $("authPass").addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") $<HTMLButtonElement>("loginBtn").click();
  });
  $("logoutBtn").addEventListener("click", async () => {
    await invoke("auth_logout");
    refresh();
  });

  // ----- learn mode -----
  $("learnGame").addEventListener("change", () => {
    selectedGame = $<HTMLSelectElement>("learnGame").value;
    refreshLearn();
  });
  $("learnToggle").addEventListener("click", async () => {
    await invoke("set_learning", { enabled: !learning });
    refresh();
  });
  $("learnClear").addEventListener("click", async () => {
    if (!selectedGame) return;
    await invoke("clear_learn", { game: selectedGame });
    for (const k of [...sel.keys()]) if (k.startsWith(selectedGame + "|")) sel.delete(k);
    refreshLearn();
  });
  $("etwAdminBtn").addEventListener("click", async () => {
    try {
      await invoke("relaunch_as_admin");
    } catch (e) {
      say("etwMsg", `✗ ${String(e)}`, "var(--red)");
    }
  });
  $("etwRetryBtn").addEventListener("click", async () => {
    await invoke("etw_restart");
    refresh();
  });
  $("learnRows").addEventListener("change", (ev) => {
    const t = ev.target as HTMLInputElement | HTMLSelectElement;
    const k = t.dataset.k;
    if (!k) return;
    const st = sel.get(k) ?? { on: false, mode: "host" as const };
    if (t.classList.contains("sel-on")) st.on = (t as HTMLInputElement).checked;
    if (t.classList.contains("sel-mode")) st.mode = t.value as "host" | "prefix";
    sel.set(k, st);
  });
  $("saveProfileBtn").addEventListener("click", async () => {
    const entries = buildEntries();
    if (!entries.length) return say("profileMsg", "select at least one server", "var(--red)");
    try {
      const p = await invoke<Profile>("save_profile", { game: selectedGame, name: null, entries });
      say("profileMsg", `✓ saved ${p.entries.length} entries`, "var(--green)");
      renderProfiles();
    } catch (e) {
      say("profileMsg", `✗ ${String(e)}`, "var(--red)");
    }
  });

  listen<State>("gpn-state", (ev) => render(ev.payload));
  refresh();
  refreshLearn();
  renderProfiles();
  setInterval(refresh, 3000);
  setInterval(refreshLearn, 3000);
});
