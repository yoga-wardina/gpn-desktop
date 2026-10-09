import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface GameProc { pid: number; name: string }
interface Target { ip: string; port?: number; proto: string }
interface GameMeta { name: string; company?: string; icon?: string; source: string }
interface State {
  procs: GameProc[];
  targets: Target[];
  watched: string[];
  gpnServerUrl: string;
  meta: Record<string, GameMeta | null>;
}

function esc(s: unknown): string {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c] as string)
  );
}

function render(s: State) {
  const dot = document.getElementById("dot")!;
  dot.classList.remove("off");
  document.getElementById("statusText")!.textContent = "monitoring";
  (document.getElementById("serverUrl") as HTMLInputElement).value = s.gpnServerUrl || "";

  const games = document.getElementById("games")!;
  games.innerHTML = s.watched.length
    ? ""
    : '<div class="empty">No games watched. Add an executable above.</div>';
  for (const exe of s.watched) {
    const live = s.procs.some((p) => p.name.toLowerCase().includes(exe.toLowerCase().split(/[\\/]/).pop() || exe));
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

  const tb = document.getElementById("targets")!;
  if (!s.targets.length) {
    tb.innerHTML = '<tr><td colspan="3" class="empty">No game traffic detected yet.</td></tr>';
  } else {
    tb.innerHTML = s.targets
      .map(
        (t) => `<tr>
          <td><span class="tag ${t.proto}">${t.proto.toUpperCase()}</span></td>
          <td class="mono">${esc(t.ip)}</td>
          <td class="mono">${t.port ?? "—"}</td>
        </tr>`
      )
      .join("");
  }
}

async function refresh() {
  try {
    render(await invoke<State>("get_state"));
  } catch (e) {
    console.error(e);
  }
}

window.addEventListener("DOMContentLoaded", () => {
  document.getElementById("watchBtn")!.addEventListener("click", async () => {
    const inp = document.getElementById("exeInput") as HTMLInputElement;
    const exe = inp.value.trim();
    if (!exe) return;
    await invoke("watch_game", { exe });
    inp.value = "";
    refresh();
  });
  document.getElementById("exeInput")!.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") (document.getElementById("watchBtn") as HTMLButtonElement).click();
  });

  document.getElementById("saveBtn")!.addEventListener("click", async () => {
    await invoke("save_settings", {
      serverUrl: (document.getElementById("serverUrl") as HTMLInputElement).value.trim(),
      token: (document.getElementById("token") as HTMLInputElement).value.trim(),
    });
    const el = document.getElementById("pingResult")!;
    el.textContent = "saved";
    el.style.color = "";
  });

  document.getElementById("pingBtn")!.addEventListener("click", async () => {
    const el = document.getElementById("pingResult")!;
    el.textContent = "testing…";
    el.style.color = "";
    try {
      await invoke("ping_server");
      el.textContent = "✓ server reachable";
      el.style.color = "var(--green)";
    } catch (e) {
      el.textContent = `✗ ${String(e)}`;
      el.style.color = "var(--red)";
    }
  });

  listen<State>("gpn-state", (ev) => render(ev.payload));
  refresh();
  setInterval(refresh, 3000);
});
