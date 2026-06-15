// Agent Sessions webview view.
//
// Receives state pushes from the extension over `postMessage` and renders
// session cards. User actions are forwarded back as command messages; the
// extension owns the side-effects (terminal launch, file I/O, dialogs).

// Mark the script as loaded immediately so the DevTools console can confirm
// it executed even if the body throws below. Helps diagnose `__aterm is not
// defined` (script not loaded vs script blew up partway).
window.__aterm = { loaded: true, error: null };

try {
(() => {
  "use strict";
  const vscode = acquireVsCodeApi();
  /** Pipe diagnostics to the extension's Output channel — works even when
   *  the user can't open DevTools for the webview iframe. */
  const diag = (label, data) =>
    vscode.postMessage({ command: "diag", label, data });
  diag("script booted");

  // ── State ────────────────────────────────────────────────────────────────
  /** @type {{
   *    providers: ProviderInfo[],
   *    sessions: Session[],
   *    metadata: Record<string, SessionMetadata>,
   *    projects: { names: Record<string,string>, colors: Record<string,string> },
   *    groupBy: "provider" | "project" | "cascade",
   *    filter: string,
   *    home: string,
   *  }} */
  let state = {
    providers: [],
    sessions: [],
    metadata: {},
    projects: { names: {}, colors: {} },
    quotas: {},
    serviceStatus: {},
    activeKeys: [],
    groupBy: "provider",
    filter: "",
    home: "",
    costAlertDaily: 0,
  };
  /** Persisted UI-only state: collapsed buckets (by key), dashboard toggle. */
  let ui = vscode.getState() || { collapsed: {}, showStats: false };
  if (ui.showStats == null) ui.showStats = false;
  // Older persisted state may predate `collapsed`; every consumer indexes it.
  if (ui.collapsed == null || typeof ui.collapsed !== "object") ui.collapsed = {};

  const NO_PROJECT = "(sin proyecto)";

  // ── Icons ─────────────────────────────────────────────────────────────────
  const ICONS = {
    chevron: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="m4 6 4 4 4-4H4Z"/></svg>`,
    play: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M4 3v10l9-5L4 3Z"/></svg>`,
    eye: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M8 4c4 0 7 4 7 4s-3 4-7 4-7-4-7-4 3-4 7-4Zm0 1.5A2.5 2.5 0 1 0 8 10.5 2.5 2.5 0 0 0 8 5.5Z"/></svg>`,
    more: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M8 5a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3Zm0 4.5A1.5 1.5 0 1 1 8 6.5a1.5 1.5 0 0 1 0 3Zm0 4.5a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3Z"/></svg>`,
    edit: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="m11.13 1.46 3.41 3.41-9.21 9.21L1.92 14.08l.83-3.41 8.38-9.21Z"/></svg>`,
    palette: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M8 1a7 7 0 1 0 0 14h.5a1.5 1.5 0 0 0 1.06-2.56 1 1 0 0 1 .7-1.71h1.74A2 2 0 0 0 14 8.73V8a7 7 0 0 0-6-7Zm-4 8a1 1 0 1 1 0-2 1 1 0 0 1 0 2Zm2-4a1 1 0 1 1 0-2 1 1 0 0 1 0 2Zm4 0a1 1 0 1 1 0-2 1 1 0 0 1 0 2Zm2 4a1 1 0 1 1 0-2 1 1 0 0 1 0 2Z"/></svg>`,
    folder: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M1.5 3a.5.5 0 0 1 .5-.5h4.41l1 1H14a.5.5 0 0 1 .5.5v8.5A1.5 1.5 0 0 1 13 14H3a1.5 1.5 0 0 1-1.5-1.5V3Z"/></svg>`,
    plus: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M8 2v5H3v2h5v5h2V9h5V7h-5V2H8Z"/></svg>`,
    terminal: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M2 2.5h12a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H2a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1Zm1.8 3-.9.9 2.2 2-2.2 2 .9.9L7 8.4 3.8 5.5ZM8 9.5h4.2v1.2H8V9.5Z"/></svg>`,
    star: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="m8 1.5 1.93 4.18 4.57.43-3.45 3.04 1.02 4.5L8 11.27 3.93 13.65l1.02-4.5L1.5 6.11l4.57-.43L8 1.5Z"/></svg>`,
    note: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M3 2h7l3 3v9H3V2Zm6.5 4V3L12 6H9.5ZM5 8h6v1H5V8Zm0 2.5h6v1H5v-1Z"/></svg>`,
  };

  // ── Provider colours (theme-aware via CSS vars) ──────────────────────────
  const PROVIDER_AVATAR = {
    claude: "var(--vscode-charts-orange)",
    codex: "var(--vscode-charts-green)",
    opencode: "var(--vscode-charts-blue)",
    gemini: "var(--vscode-charts-purple)",
  };
  const PROVIDER_INITIAL = {
    claude: "C",
    codex: "X",
    opencode: "O",
    gemini: "G",
  };

  // ── Helpers ──────────────────────────────────────────────────────────────
  const el = (tag, attrs = {}, children = []) => {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (v === false || v == null) continue;
      if (k === "class") n.className = v;
      else if (k === "html") n.innerHTML = v;
      else if (k === "text") n.textContent = v;
      else if (k.startsWith("on") && typeof v === "function")
        n.addEventListener(k.slice(2).toLowerCase(), v);
      else if (k.startsWith("data-")) n.setAttribute(k, String(v));
      else if (k === "style" && typeof v === "object") Object.assign(n.style, v);
      else n.setAttribute(k, String(v));
    }
    for (const c of [].concat(children)) {
      if (c == null || c === false) continue;
      n.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    }
    return n;
  };

  const displayPath = (p) => {
    if (!state.home) return p;
    if (p === state.home) return "~";
    if (p.startsWith(state.home + "/") || p.startsWith(state.home + "\\")) {
      return "~" + p.slice(state.home.length);
    }
    return p;
  };
  const basename = (p) => {
    if (!p) return "";
    const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
    return i >= 0 ? p.slice(i + 1) : p;
  };
  const parentOf = (p) => {
    const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
    return i > 0 ? p.slice(0, i) : p;
  };
  const shortModel = (m) => {
    if (!m) return "";
    const i = m.lastIndexOf("/");
    return i >= 0 ? m.slice(i + 1) : m;
  };
  const relativeTime = (unix) => {
    const secs = Math.max(0, Date.now() / 1000 - unix);
    const m = Math.floor(secs / 60);
    if (m < 1) return "ahora";
    if (m < 60) return `hace ${m} min`;
    const h = Math.floor(m / 60);
    if (h < 24) return `hace ${h} h`;
    return `hace ${Math.floor(h / 24)} d`;
  };
  const metaFor = (s) => state.metadata[`${s.provider}:${s.id}`] || null;
  const projectKey = (s) => s.cwd || NO_PROJECT;

  /** Whitespace-separated AND of predicates. Three kinds:
   *   - `#tag`           — exact tag match (case-insensitive).
   *   - `key:value`      — field predicate (provider:claude, model:opus,
   *                        cwd:aterm, branch:main, has:notes, has:favorite).
   *   - `key<n` / `key>n`— numeric comparison (cost>5, tokens>100000,
   *                        msgs<10, age>7).
   *   - everything else  — substring against title/name/cwd/branch/tags.
   *  Empty query matches everything. */
  const matchesFilter = (s, q) => {
    if (!q) return true;
    const m = metaFor(s);
    const tags = ((m && m.tags) || []).map((t) => t.toLowerCase());
    const tokens = q.split(/\s+/).filter(Boolean);
    for (const tok of tokens) {
      if (!matchToken(s, m, tags, tok)) return false;
    }
    return true;
  };

  function matchToken(s, m, tags, tok) {
    const lo = tok.toLowerCase();
    // Tag predicate.
    if (lo.startsWith("#") && lo.length > 1) {
      return tags.includes(lo.slice(1));
    }
    // Numeric comparison: key<n / key>n.
    const cmp = lo.match(/^([a-z]+)([<>])([0-9.]+)$/);
    if (cmp) {
      const [, key, op, raw] = cmp;
      const n = parseFloat(raw);
      const v = numericField(s, key);
      if (v == null) return false;
      return op === "<" ? v < n : v > n;
    }
    // Key:value predicate.
    const kv = lo.match(/^([a-z]+):(.+)$/);
    if (kv) {
      const [, key, value] = kv;
      return matchKeyValue(s, m, tags, key, value);
    }
    // Plain substring against the haystack.
    const hay = [
      s.title || "",
      m && m.name ? m.name : "",
      s.cwd || "",
      s.branch || "",
      ...tags,
    ]
      .join("\n")
      .toLowerCase();
    return hay.includes(lo);
  }

  function numericField(s, key) {
    if (key === "cost") return s.costUsd;
    if (key === "tokens") return s.contextTokens;
    if (key === "msgs" || key === "messages") return s.messageCount;
    if (key === "age") {
      // age in days since last activity
      return (Date.now() / 1000 - s.lastActivity) / 86400;
    }
    if (key === "ctx" || key === "context") {
      // % context used (relative to window)
      if (s.contextTokens && s.contextWindow)
        return (s.contextTokens / s.contextWindow) * 100;
      return null;
    }
    return null;
  }

  function matchKeyValue(s, m, tags, key, value) {
    const v = (x) => (x || "").toLowerCase().includes(value);
    switch (key) {
      case "provider":
      case "p":
        return (s.provider || "").toLowerCase() === value;
      case "model":
      case "m":
        return v(s.model || "");
      case "cwd":
      case "path":
        return v(s.cwd || "");
      case "branch":
      case "b":
        return v(s.branch || "");
      case "title":
      case "name":
        return v(s.title || "") || v((m && m.name) || "");
      case "tag":
        return tags.includes(value);
      case "has": {
        if (value === "notes") return !!(m && m.notes);
        if (value === "favorite" || value === "fav") return !!(m && m.favorite);
        if (value === "tags") return tags.length > 0;
        if (value === "color") return !!(m && m.color);
        if (value === "model") return !!s.model;
        if (value === "branch") return !!s.branch;
        return false;
      }
      case "active":
        return value === "true" ? !!s.isActive : !s.isActive;
      default:
        return false;
    }
  }

  // ── Sidecar messaging ────────────────────────────────────────────────────
  const post = (command, payload) =>
    vscode.postMessage({ command, ...(payload || {}) });

  // ── Rendering ────────────────────────────────────────────────────────────
  const root = document.getElementById("tree");
  const emptyView = document.getElementById("empty");
  const statsView = document.getElementById("stats");

  function render() {
    try {
      doRender();
      // Report what actually ended up in the DOM so we can tell apart
      // "rendered nothing" from "rendered but invisible".
      diag("render done", {
        buckets: document.querySelectorAll(".bucket").length,
        cards: document.querySelectorAll(".card").length,
        emptyHidden: emptyView.hidden,
        rootHidden: root.hidden,
        statsHidden: statsView.hidden,
      });
    } catch (e) {
      const msg = e && (e.stack || e.message || String(e));
      diag("render error", msg);
      console.error("[agentSessions] render falló:", e);
      root.hidden = false;
      statsView.hidden = true;
      root.innerHTML = "";
      root.appendChild(
        el("div", {
          class: "filter-banner",
          style: { color: "var(--vscode-charts-red)" },
          html: `<strong>Error al renderizar:</strong>
                 <span class="count">${escapeHtml(String(e && e.message || e))}</span>`,
        })
      );
    }
  }

  function doRender() {
    // Stats view replaces the tree when toggled on.
    if (ui.showStats) {
      root.hidden = true;
      emptyView.hidden = true;
      statsView.hidden = false;
      renderStats();
      return;
    }
    root.hidden = false;
    statsView.hidden = true;

    const prevScroll = root.scrollTop;
    root.innerHTML = "";

    // Apply filter banner (visible only when active).
    const filteredSessions = state.sessions.filter((s) =>
      matchesFilter(s, state.filter)
    );

    if (state.filter) {
      root.appendChild(
        el("div", {
          class: "filter-banner",
          onClick: () => post("setFilter", { value: "" }),
          html: `<span>Filtro: <strong>${escapeHtml(state.filter)}</strong></span>
                 <span class="count">${filteredSessions.length} resultado(s) · clic para limpiar</span>`,
        })
      );
    }

    // Cost alert: total cost of sessions whose last activity was today vs the
    // configured daily threshold. Only shown when the threshold is set (>0).
    if (state.costAlertDaily > 0) {
      const today = todaysCost();
      if (today >= state.costAlertDaily) {
        root.appendChild(
          el("div", {
            class: "cost-alert",
            title: `Umbral diario configurado: $${state.costAlertDaily}`,
            html: `<strong>⚠ Alerta de coste</strong>
                   <span>Hoy llevas $${today.toFixed(2)} (umbral $${state.costAlertDaily}).</span>`,
          })
        );
      }
    }

    if (filteredSessions.length === 0) {
      emptyView.hidden = false;
      // Rewrite the empty view to be diagnostic instead of generic — tells
      // the user *why* it's empty (no scan yet, filter active, …).
      const total = state.sessions.length;
      const provs = state.providers.length;
      emptyView.innerHTML = "";
      if (provs === 0 && total === 0) {
        emptyView.appendChild(
          el("p", { text: "Esperando datos del sidecar…" })
        );
        emptyView.appendChild(
          el("p", {
            class: "hint",
            text: "Si tarda más de unos segundos, comprueba que `agent-sessions-cli` está disponible.",
          })
        );
      } else if (state.filter) {
        emptyView.appendChild(
          el("p", {
            text: `Ningún resultado para "${state.filter}".`,
          })
        );
      } else {
        emptyView.appendChild(
          el("p", { text: "No se encontraron sesiones de agentes." })
        );
        emptyView.appendChild(
          el("p", {
            class: "hint",
            text: `Sidecar reporta ${provs} proveedor(es) y ${total} sesión(es), pero ninguna pasa el grupo actual.`,
          })
        );
      }
      emptyView.appendChild(
        el("button", {
          class: "primary",
          text: "Refrescar",
          onClick: () => post("refresh"),
        })
      );
      return;
    }
    emptyView.hidden = true;

    const groupBy = state.groupBy;
    if (groupBy === "project") {
      renderProjectBuckets(filteredSessions, true);
    } else if (groupBy === "cascade") {
      renderProviderBuckets(filteredSessions, true);
    } else if (groupBy === "date") {
      renderDateBuckets(filteredSessions);
    } else {
      renderProviderBuckets(filteredSessions, false);
    }

    // Restore scroll position so toggle/state-push doesn't kick the user back
    // to the top of the panel.
    root.scrollTop = prevScroll;
  }

  function renderProviderBuckets(sessions, cascade) {
    for (const info of state.providers) {
      const ps = sessions.filter((s) => s.provider === info.id);
      if (ps.length === 0) continue;
      const key = `provider:${info.id}`;
      const collapsed = !!ui.collapsed[key];
      const header = bucketHeader({
        key,
        label: info.displayName.toUpperCase(),
        count: ps.length,
        accentVar: PROVIDER_AVATAR[info.id] || "var(--vscode-charts-foreground)",
        quota: state.quotas[info.id],
        serviceStatus: state.serviceStatus[info.id],
        stateCounts: countStates(ps),
        collapsed,
      });
      root.appendChild(header);
      if (collapsed) continue;
      if (cascade) {
        const groups = bucketByProject(ps);
        for (const [cwd, items] of groups) {
          const k2 = `${key}/project:${cwd}`;
          const c2 = !!ui.collapsed[k2];
          root.appendChild(
            bucketHeader({
              key: k2,
              label: projectLabel(cwd),
              count: items.length,
              nested: true,
              accentVar: projectAccentVar(cwd),
              cwd,
              collapsed: c2,
            })
          );
          if (!c2) root.appendChild(cardList(items));
        }
      } else {
        root.appendChild(cardList(ps));
      }
    }
  }

  function renderDateBuckets(sessions) {
    // Stable order of date buckets, regardless of which ones have content.
    const order = [
      "today",
      "yesterday",
      "this-week",
      "this-month",
      "older",
    ];
    const labels = {
      today: "HOY",
      yesterday: "AYER",
      "this-week": "ESTA SEMANA",
      "this-month": "ESTE MES",
      older: "MÁS ANTIGUO",
    };
    const buckets = new Map(order.map((k) => [k, []]));
    for (const s of sessions) buckets.get(dateBucket(s.lastActivity)).push(s);
    for (const k of order) {
      const items = buckets.get(k);
      if (!items.length) continue;
      const key = `date:${k}`;
      const collapsed = !!ui.collapsed[key];
      root.appendChild(
        bucketHeader({
          key,
          label: labels[k],
          count: items.length,
          collapsed,
        })
      );
      if (!collapsed) root.appendChild(cardList(items));
    }
  }

  /** Map a unix timestamp to one of five labelled date buckets. Uses local
   *  time and an ISO week (Monday-start) for "this week", matching what a
   *  user expects without dragging in a date library. */
  function dateBucket(unix) {
    const now = new Date();
    const at = new Date(unix * 1000);
    const sameDay = (a, b) =>
      a.getFullYear() === b.getFullYear() &&
      a.getMonth() === b.getMonth() &&
      a.getDate() === b.getDate();
    if (sameDay(at, now)) return "today";
    const yest = new Date(now);
    yest.setDate(yest.getDate() - 1);
    if (sameDay(at, yest)) return "yesterday";
    // Monday-start week.
    const monday = new Date(now);
    const dow = (monday.getDay() + 6) % 7; // 0 = Monday
    monday.setHours(0, 0, 0, 0);
    monday.setDate(monday.getDate() - dow);
    if (at >= monday) return "this-week";
    const firstOfMonth = new Date(now.getFullYear(), now.getMonth(), 1);
    if (at >= firstOfMonth) return "this-month";
    return "older";
  }

  function renderProjectBuckets(sessions, /*root*/ _root) {
    const groups = bucketByProject(sessions);
    for (const [cwd, items] of groups) {
      const key = `project:${cwd}`;
      const collapsed = !!ui.collapsed[key];
      root.appendChild(
        bucketHeader({
          key,
          label: projectLabel(cwd).toUpperCase(),
          count: items.length,
          accentVar: projectAccentVar(cwd),
          cwd,
          collapsed,
        })
      );
      if (collapsed) continue;
      root.appendChild(cardList(items));
    }
  }

  /** Keep ordering stable by first-seen (sessions arrive newest-first, so
   *  buckets end up sorted by most-recent activity). */
  function bucketByProject(sessions) {
    const order = [];
    const map = new Map();
    for (const s of sessions) {
      const k = projectKey(s);
      if (!map.has(k)) {
        map.set(k, []);
        order.push(k);
      }
      map.get(k).push(s);
    }
    return order.map((k) => [k, map.get(k)]);
  }

  function projectLabel(cwd) {
    if (cwd === NO_PROJECT) return NO_PROJECT;
    const alias = state.projects.names[cwd];
    return (alias && alias.trim()) || basename(cwd) || cwd;
  }
  function projectAccentVar(cwd) {
    const hex = state.projects.colors[cwd];
    return hex ? hex : "";
  }

  function bucketHeader({
    key,
    label,
    count,
    nested = false,
    accentVar = "",
    cwd = null,
    quota = null,
    serviceStatus = null,
    stateCounts = null,
    collapsed = false,
  }) {
    const isDropTarget = cwd && cwd !== NO_PROJECT;
    const node = el("div", {
      class: `bucket ${nested ? "nested" : ""} ${collapsed ? "collapsed" : ""}`,
      role: "treeitem",
      "aria-expanded": String(!collapsed),
      tabindex: "0",
      "data-key": key,
      onClick: () => toggleBucket(key),
      onKeydown: (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          toggleBucket(key);
        }
      },
      onDragover: isDropTarget
        ? (e) => {
            if (!e.dataTransfer.types.includes("application/x-agent-session"))
              return;
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
            node.classList.add("drop-target");
          }
        : undefined,
      onDragleave: isDropTarget
        ? () => node.classList.remove("drop-target")
        : undefined,
      onDrop: isDropTarget
        ? (e) => {
            node.classList.remove("drop-target");
            const raw = e.dataTransfer.getData("application/x-agent-session");
            if (!raw) return;
            e.preventDefault();
            try {
              const data = JSON.parse(raw);
              if (data.sourceCwd && data.sourceCwd !== cwd) {
                post("moveSession", {
                  id: data.id,
                  sourceCwd: data.sourceCwd,
                  destCwd: cwd,
                });
              }
            } catch (_) {
              /* ignore malformed payloads */
            }
          }
        : undefined,
    });
    node.appendChild(el("span", { class: "chevron", html: ICONS.chevron }));
    if (accentVar) {
      node.appendChild(
        el("span", { class: "swatch", style: { background: accentVar } })
      );
    }
    node.appendChild(el("span", { class: "name", text: label }));
    node.appendChild(
      el("span", {
        class: "meta",
        text: count === 1 ? "1 sesión" : `${count} sesiones`,
      })
    );
    if (stateCounts) {
      const pills = stateCountPills(stateCounts);
      pills.forEach((p) => node.appendChild(p));
    }
    if (serviceStatus) node.appendChild(statusDot(serviceStatus));
    if (quota) {
      quotaPills(quota).forEach((p) => node.appendChild(p));
    }
    if (cwd && cwd !== NO_PROJECT) {
      // Inline actions for project buckets: launch here, terminal here, edit.
      const actions = el("span", { class: "actions" });
      actions.appendChild(
        actionBtn("Nueva sesión aquí", ICONS.plus, (e) => {
          e.stopPropagation();
          post("newSession", { cwd });
        })
      );
      actions.appendChild(
        actionBtn("Abrir terminal aquí", ICONS.terminal, (e) => {
          e.stopPropagation();
          post("openTerminal", { cwd });
        })
      );
      actions.appendChild(
        actionBtn("Renombrar proyecto", ICONS.edit, (e) => {
          e.stopPropagation();
          post("renameProject", { cwd });
        })
      );
      actions.appendChild(
        actionBtn("Color del proyecto", ICONS.palette, (e) => {
          e.stopPropagation();
          post("setProjectColor", { cwd });
        })
      );
      node.appendChild(actions);
    }
    return node;
  }

  /** Tally per-state of the live sessions inside a provider bucket. Only
   *  active sessions count; the rest are historical. */
  function countStates(sessions) {
    let working = 0,
      waiting = 0,
      other = 0;
    for (const s of sessions) {
      if (!s.isActive) continue;
      if (s.liveStatus === "busy") working++;
      else if (s.liveStatus === "idle") waiting++;
      else other++;
    }
    return { working, waiting, other };
  }

  /** Three small coloured pills `●N` for the active-session tally. Returns
   *  an array of nodes (some may be empty) so the caller can append them all. */
  function stateCountPills(c) {
    const pills = [];
    if (c.working > 0) {
      pills.push(
        el("span", {
          class: "state-pill",
          title: "Trabajando",
          style: { color: "var(--vscode-charts-orange)" },
          html: `● ${c.working}`,
        })
      );
    }
    if (c.waiting > 0) {
      pills.push(
        el("span", {
          class: "state-pill",
          title: "Esperando input",
          style: { color: "var(--vscode-charts-green)" },
          html: `● ${c.waiting}`,
        })
      );
    }
    if (c.other > 0) {
      pills.push(
        el("span", {
          class: "state-pill",
          title: "Activas",
          style: { color: "var(--vscode-charts-blue)" },
          html: `● ${c.other}`,
        })
      );
    }
    return pills;
  }

  /** Coloured dot (●) for the provider's statuspage indicator. Always shown
   *  when we have a status: green when operational, escalating to red for
   *  critical. The tooltip carries the description ("All Systems
   *  Operational", "Partial outage", …). */
  function statusDot(status) {
    return el("span", {
      class: "status-dot",
      title: `${status.indicator}: ${status.description}`,
      style: { color: statusColor(status.indicator) },
      html: "●",
    });
  }
  function statusColor(ind) {
    switch (ind) {
      case "none":
        return "var(--vscode-charts-green)";
      case "minor":
        return "var(--vscode-charts-yellow)";
      case "major":
        return "var(--vscode-charts-orange)";
      case "critical":
        return "var(--vscode-charts-red)";
      default:
        return "var(--vscode-descriptionForeground)";
    }
  }

  /** One small pill per quota window (the rolling ~5h "session" and the
   *  weekly cap, when the provider reports them). The prefix shows the
   *  *time remaining* until the window resets — far more actionable than
   *  the window's fixed name ("5h" doesn't tell you anything; "2h15m" does).
   *  Falls back to the window label if no reset timestamp is available. */
  function quotaPills(quota) {
    if (!quota.windows || quota.windows.length === 0) return [];
    return quota.windows.map((w) => {
      const pct = Math.round(w.usedPercent);
      let tone = "var(--vscode-charts-green)";
      if (pct >= 80) tone = "var(--vscode-charts-red)";
      else if (pct >= 60) tone = "var(--vscode-charts-orange)";
      else if (pct >= 40) tone = "var(--vscode-charts-yellow)";
      const left = w.resetsAt ? remainingShort(w.resetsAt) : quotaWindowShort(w.label);
      const tooltipReset = w.resetsAt
        ? ` · reset en ${remainingLong(w.resetsAt)} (${new Date(
            w.resetsAt * 1000
          ).toLocaleString()})`
        : "";
      return el("span", {
        class: "quota",
        title: `${quotaWindowName(w.label)}: ${pct}%${tooltipReset}`,
        style: { color: tone },
        html: `${left} ${pct}%`,
      });
    });
  }

  /** Compact "time remaining" for the pill: `42m`, `2h15m`, `3d4h`, `<1m`,
   *  or `ya` if the timestamp is in the past. Designed to fit inside a tight
   *  pill at ~5 chars max. */
  function remainingShort(unixSeconds) {
    const diff = unixSeconds - Date.now() / 1000;
    if (diff <= 0) return "ya";
    if (diff < 60) return "<1m";
    const mins = Math.floor(diff / 60);
    if (mins < 60) return `${mins}m`;
    const hours = Math.floor(mins / 60);
    const remMins = mins % 60;
    if (hours < 24) {
      return remMins > 0 ? `${hours}h${remMins}m` : `${hours}h`;
    }
    const days = Math.floor(hours / 24);
    const remHours = hours % 24;
    return remHours > 0 ? `${days}d${remHours}h` : `${days}d`;
  }
  /** Verbose form for tooltips: `2 horas 15 minutos` etc. */
  function remainingLong(unixSeconds) {
    const diff = unixSeconds - Date.now() / 1000;
    if (diff <= 0) return "ya";
    if (diff < 60) return "menos de un minuto";
    const mins = Math.floor(diff / 60);
    if (mins < 60) return `${mins} minuto${mins === 1 ? "" : "s"}`;
    const hours = Math.floor(mins / 60);
    const remMins = mins % 60;
    if (hours < 24) {
      const h = `${hours} hora${hours === 1 ? "" : "s"}`;
      return remMins > 0
        ? `${h} y ${remMins} minuto${remMins === 1 ? "" : "s"}`
        : h;
    }
    const days = Math.floor(hours / 24);
    const remHours = hours % 24;
    const d = `${days} día${days === 1 ? "" : "s"}`;
    return remHours > 0
      ? `${d} y ${remHours} hora${remHours === 1 ? "" : "s"}`
      : d;
  }
  /** Last-resort label when no reset time is known. */
  function quotaWindowShort(label) {
    switch ((label || "").toLowerCase()) {
      case "session":
        return "5h";
      case "weekly":
      case "week":
        return "7d";
      default:
        return (label || "?").slice(0, 1).toUpperCase();
    }
  }
  function quotaWindowName(label) {
    switch ((label || "").toLowerCase()) {
      case "session":
        return "Ventana 5h";
      case "weekly":
      case "week":
        return "Ventana semanal";
      default:
        return label || "Ventana";
    }
  }

  function toggleBucket(key) {
    ui.collapsed[key] = !ui.collapsed[key];
    vscode.setState(ui);
    render();
    // Keep keyboard focus on the same header after re-render.
    requestAnimationFrame(() => {
      const next = document.querySelector(
        `.bucket[data-key="${cssEscape(key)}"]`
      );
      if (next instanceof HTMLElement) next.focus();
    });
  }

  function cardList(sessions) {
    // Pin favourites to the top inside each bucket; otherwise preserve the
    // scan order (most-recent first).
    const sorted = sessions.slice().sort((a, b) => {
      const fa = !!(metaFor(a) && metaFor(a).favorite);
      const fb = !!(metaFor(b) && metaFor(b).favorite);
      if (fa !== fb) return fa ? -1 : 1;
      return 0;
    });
    const list = el("div", { class: "cards", role: "group" });
    for (const s of sorted) list.appendChild(card(s));
    return list;
  }

  function card(s) {
    const m = metaFor(s);
    const title =
      (m && m.name && m.name.trim()) ||
      (s.title && s.title.trim()) ||
      `(sin título) ${s.id.slice(0, 8)}`;
    const accent = s.cwd ? state.projects.colors[s.cwd] : null;
    const inUse = state.activeKeys.includes(`${s.provider}:${s.id}`);
    // Only Claude's on-disk layout is supported by `move_session` today.
    const dragOk = s.provider === "claude" && !!s.cwd;
    const node = el("div", {
      class: `card ${inUse ? "in-use" : ""}`,
      role: "treeitem",
      tabindex: "0",
      "data-session-id": s.id,
      "data-provider": s.provider,
      draggable: dragOk ? "true" : "false",
      title: tooltipText(s, m, inUse),
      style: accent ? { "--card-accent": accent } : undefined,
      onClick: () => post("resume", { provider: s.provider, id: s.id }),
      onKeydown: (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          post("resume", { provider: s.provider, id: s.id });
        }
      },
      onContextmenu: (e) => {
        e.preventDefault();
        post("contextMenu", { provider: s.provider, id: s.id });
      },
      onDragstart: dragOk
        ? (e) => {
            const payload = JSON.stringify({
              id: s.id,
              sourceCwd: s.cwd,
            });
            // Custom MIME so we recognise our own payload on drop without
            // racing against text being dragged in from outside.
            e.dataTransfer.setData("application/x-agent-session", payload);
            e.dataTransfer.effectAllowed = "move";
            node.classList.add("dragging");
          }
        : undefined,
      onDragend: dragOk
        ? () => node.classList.remove("dragging")
        : undefined,
    });

    // Avatar: provider initial in a coloured circle; green dot if live.
    node.appendChild(
      el("span", {
        class: `avatar ${s.isActive ? "live" : ""}`,
        style: {
          "--avatar-bg":
            PROVIDER_AVATAR[s.provider] || "var(--vscode-charts-foreground)",
        },
        text: PROVIDER_INITIAL[s.provider] || s.provider[0].toUpperCase(),
      })
    );

    node.appendChild(el("span", { class: "title", text: title }));

    const metaBits = [relativeTime(s.lastActivity)];
    if (s.model) metaBits.push(shortModel(s.model));
    if (s.branch) metaBits.push(s.branch);
    if (s.cwd && !accent) metaBits.push(basename(s.cwd));
    const meta = el("span", { class: "meta" });
    metaBits.forEach((b, i) => {
      if (i > 0) meta.appendChild(el("span", { class: "sep", text: "·" }));
      meta.appendChild(el("span", { text: b }));
    });
    // Context % pill: how much of the model's window the latest turn used.
    // Colour-coded the same way as the quota badge (>= 60% warning, >= 80% hot).
    if (s.contextTokens && s.contextWindow) {
      const pct = Math.round((s.contextTokens / s.contextWindow) * 100);
      let tone = "var(--vscode-charts-green)";
      if (pct >= 80) tone = "var(--vscode-charts-red)";
      else if (pct >= 60) tone = "var(--vscode-charts-orange)";
      else if (pct >= 40) tone = "var(--vscode-charts-yellow)";
      meta.appendChild(el("span", { class: "sep", text: "·" }));
      meta.appendChild(
        el("span", {
          class: "ctx-pct",
          style: { color: tone },
          title: `${s.contextTokens.toLocaleString()} / ${s.contextWindow.toLocaleString()} tokens`,
          text: `ctx ${pct}%`,
        })
      );
    }
    if (m && m.tags && m.tags.length) {
      const tags = el("span", { class: "badges" });
      for (const t of m.tags) {
        tags.appendChild(
          el("button", {
            class: "tag",
            text: `#${t}`,
            title: `Filtrar por #${t}`,
            onClick: (e) => {
              e.stopPropagation();
              toggleTagFilter(t);
            },
          })
        );
      }
      meta.appendChild(tags);
    }
    node.appendChild(meta);

    // Star (favourite toggle), always visible if favourite, on hover otherwise.
    if (m && m.favorite) {
      node.appendChild(
        el("span", {
          class: "star pinned",
          title: "Favorito (clic para quitar)",
          html: ICONS.star,
          onClick: (e) => {
            e.stopPropagation();
            post("toggleFavorite", { provider: s.provider, id: s.id });
          },
        })
      );
    }
    // A subtle pencil-tip if the session has notes.
    if (m && m.notes) {
      node.appendChild(
        el("span", {
          class: "note-indicator",
          title: m.notes,
          html: ICONS.note,
        })
      );
    }

    // Hover actions (right side).
    const actions = el("span", { class: "actions" });
    if (!(m && m.favorite)) {
      actions.appendChild(
        actionBtn("Marcar favorito", ICONS.star, (e) => {
          e.stopPropagation();
          post("toggleFavorite", { provider: s.provider, id: s.id });
        })
      );
    }
    actions.appendChild(
      actionBtn("Reanudar", ICONS.play, (e) => {
        e.stopPropagation();
        post("resume", { provider: s.provider, id: s.id });
      })
    );
    actions.appendChild(
      actionBtn("Previsualizar", ICONS.eye, (e) => {
        e.stopPropagation();
        post("preview", { provider: s.provider, id: s.id });
      })
    );
    actions.appendChild(
      actionBtn("Más…", ICONS.more, (e) => {
        e.stopPropagation();
        post("contextMenu", { provider: s.provider, id: s.id });
      })
    );
    node.appendChild(actions);

    return node;
  }

  /** Add `#tag` to the filter, or remove it if it's already there. The
   *  matchesFilter() helper treats `#name` tokens as exact-tag predicates. */
  // Clicking a tag badge on a card toggles the same `#tag` token as the header
  // tag popover. Delegating to toggleToken keeps the quick-filter buttons and
  // the tag menu in sync (applyFilter → updateQuickFilters).
  function toggleTagFilter(tag) {
    toggleToken(`#${tag}`);
  }

  // ── Stats / dashboard ────────────────────────────────────────────────────

  function renderStats() {
    statsView.innerHTML = "";
    const sessions = state.sessions;
    if (sessions.length === 0) {
      statsView.appendChild(
        el("p", { class: "stats-empty", text: "Aún no hay datos para mostrar." })
      );
      return;
    }
    const now = Date.now() / 1000;
    const SEVEN_DAYS = 7 * 86400;
    const recent = sessions.filter((s) => now - s.lastActivity < SEVEN_DAYS);
    const totalCost = sessions.reduce((a, s) => a + (s.costUsd || 0), 0);
    const recentCost = recent.reduce((a, s) => a + (s.costUsd || 0), 0);
    const totalTokens = sessions.reduce((a, s) => a + (s.contextTokens || 0), 0);

    // KPI row.
    const kpis = el("div", { class: "kpi-row" }, [
      kpi("Sesiones", `${sessions.length}`, `${recent.length} esta semana`),
      kpi("Coste", fmtUsd(totalCost), `${fmtUsd(recentCost)} esta semana`),
      kpi("Tokens", fmtTokens(totalTokens), "del último turno · acumulado"),
    ]);
    statsView.appendChild(kpis);

    // Sessions-per-provider bar chart.
    const byProvider = new Map();
    for (const s of sessions) {
      const entry = byProvider.get(s.provider) || { count: 0, cost: 0 };
      entry.count++;
      entry.cost += s.costUsd || 0;
      byProvider.set(s.provider, entry);
    }
    const providerRows = [...byProvider.entries()]
      .sort((a, b) => b[1].count - a[1].count);
    if (providerRows.length) {
      const max = providerRows[0][1].count;
      statsView.appendChild(sectionTitle("Por proveedor"));
      const bars = el("div", { class: "bars" });
      for (const [id, v] of providerRows) {
        const info = state.providers.find((p) => p.id === id);
        bars.appendChild(
          barRow({
            label: info ? info.displayName : id,
            value: v.count,
            extra: fmtUsd(v.cost),
            ratio: v.count / max,
            color: PROVIDER_AVATAR[id] || "var(--vscode-charts-foreground)",
          })
        );
      }
      statsView.appendChild(bars);
    }

    // Top projects by session count.
    const byProject = new Map();
    for (const s of sessions) {
      if (!s.cwd) continue;
      const entry = byProject.get(s.cwd) || { count: 0, cost: 0 };
      entry.count++;
      entry.cost += s.costUsd || 0;
      byProject.set(s.cwd, entry);
    }
    const projectRows = [...byProject.entries()]
      .sort((a, b) => b[1].count - a[1].count)
      .slice(0, 5);
    if (projectRows.length) {
      const max = projectRows[0][1].count;
      statsView.appendChild(sectionTitle("Top proyectos"));
      const bars = el("div", { class: "bars" });
      for (const [cwd, v] of projectRows) {
        const alias = state.projects.names[cwd];
        bars.appendChild(
          barRow({
            label: (alias && alias.trim()) || basename(cwd) || cwd,
            sub: displayPath(cwd),
            value: v.count,
            extra: fmtUsd(v.cost),
            ratio: v.count / max,
            color:
              state.projects.colors[cwd] || "var(--vscode-charts-blue)",
          })
        );
      }
      statsView.appendChild(bars);
    }

    // 30-day activity sparkline.
    statsView.appendChild(sectionTitle("Actividad — 30 días"));
    statsView.appendChild(sparkline(sessions));
  }

  function kpi(label, value, sub) {
    return el("div", { class: "kpi" }, [
      el("div", { class: "kpi-value", text: value }),
      el("div", { class: "kpi-label", text: label }),
      sub ? el("div", { class: "kpi-sub", text: sub }) : null,
    ]);
  }
  function sectionTitle(text) {
    return el("h3", { class: "stats-title", text });
  }
  function barRow({ label, sub, value, extra, ratio, color }) {
    return el("div", { class: "bar-row" }, [
      el("div", { class: "bar-head" }, [
        el("span", { class: "bar-label", text: label }),
        sub ? el("span", { class: "bar-sub", text: sub }) : null,
      ]),
      el("div", { class: "bar-track" }, [
        el("div", {
          class: "bar-fill",
          style: { width: `${Math.max(2, ratio * 100)}%`, background: color },
        }),
      ]),
      el("div", { class: "bar-tail" }, [
        el("span", { class: "bar-value", text: value.toString() }),
        extra ? el("span", { class: "bar-extra", text: extra }) : null,
      ]),
    ]);
  }
  function sparkline(sessions) {
    const days = 30;
    const counts = new Array(days).fill(0);
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    for (const s of sessions) {
      const d = new Date(s.lastActivity * 1000);
      d.setHours(0, 0, 0, 0);
      const delta = Math.round((today - d) / 86400000);
      if (delta >= 0 && delta < days) counts[days - 1 - delta]++;
    }
    const max = Math.max(1, ...counts);
    const w = 300;
    const h = 60;
    const dx = w / (days - 1);
    const points = counts
      .map((c, i) => `${(i * dx).toFixed(1)},${(h - (c / max) * (h - 4) - 2).toFixed(1)}`)
      .join(" ");
    const area = `0,${h} ${points} ${w},${h}`;
    const svg = `
      <svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" class="spark">
        <polygon points="${area}" fill="var(--vscode-charts-blue)" opacity="0.18" />
        <polyline points="${points}" fill="none" stroke="var(--vscode-charts-blue)" stroke-width="1.5" />
      </svg>`;
    const wrap = el("div", { class: "sparkline", html: svg });
    const summary = el("div", { class: "spark-summary" }, [
      el("span", { text: `Total: ${counts.reduce((a, b) => a + b, 0)}` }),
      el("span", { text: `Pico: ${max} / día` }),
    ]);
    return el("div", { class: "sparkline-wrap" }, [wrap, summary]);
  }
  /** Sum cost of sessions whose lastActivity falls in today (local). */
  function todaysCost() {
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const since = today.getTime() / 1000;
    let total = 0;
    for (const s of state.sessions) {
      if (s.lastActivity >= since && s.costUsd) total += s.costUsd;
    }
    return total;
  }

  function fmtUsd(n) {
    if (!n) return "$0";
    if (n >= 100) return `$${n.toFixed(0)}`;
    if (n >= 1) return `$${n.toFixed(2)}`;
    return `$${n.toFixed(3)}`;
  }
  function fmtTokens(n) {
    if (!n) return "0";
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
    return String(n);
  }

  function actionBtn(title, svg, onClick) {
    return el("button", { class: "icon-btn", title, onClick, html: svg });
  }

  function tooltipText(s, m, inUse = false) {
    const lines = [];
    if (m && m.name) lines.push(`${m.name} — ${s.title || s.id.slice(0, 8)}`);
    else lines.push(s.title || s.id);
    if (inUse) lines.push("● En uso en otro terminal — clic para enfocarlo");
    lines.push(`Proveedor: ${s.provider}`);
    if (s.model) lines.push(`Modelo: ${s.model}`);
    if (s.cwd) lines.push(`Directorio: ${displayPath(s.cwd)}`);
    if (s.branch) lines.push(`Rama: ${s.branch}`);
    if (s.messageCount != null) lines.push(`Mensajes: ${s.messageCount}`);
    if (m && m.tags && m.tags.length)
      lines.push(`Etiquetas: ${m.tags.join(", ")}`);
    lines.push(`Última: ${new Date(s.lastActivity * 1000).toLocaleString()}`);
    return lines.join("\n");
  }

  function escapeHtml(s) {
    return s
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }
  function cssEscape(s) {
    return String(s).replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
  }

  // ── Filter-token helpers ───────────────────────────────────────────────
  // The filter is a whitespace-separated AND of predicates. The quick-filter
  // buttons just toggle individual tokens (e.g. `active:true`, `#wip`) in/out
  // of that string, so they compose with whatever the user typed.
  const filterTokens = (q) => (q || "").split(/\s+/).filter(Boolean);
  const hasToken = (tok) =>
    filterTokens(state.filter).some((t) => t.toLowerCase() === tok.toLowerCase());

  function applyFilter(value) {
    state.filter = value;
    filterInput.value = value;
    clearBtn.hidden = value.length === 0;
    updateQuickFilters();
    render();
    post("filterChanged", { value });
  }
  function toggleToken(tok) {
    const lo = tok.toLowerCase();
    const toks = filterTokens(state.filter);
    const i = toks.findIndex((t) => t.toLowerCase() === lo);
    if (i >= 0) toks.splice(i, 1);
    else toks.push(tok);
    applyFilter(toks.join(" "));
  }

  /** Distinct tags assigned across all sessions, with usage counts, sorted. */
  function usedTags() {
    const counts = new Map();
    for (const s of state.sessions) {
      const m = metaFor(s);
      if (m && m.tags)
        for (const t of m.tags) counts.set(t, (counts.get(t) || 0) + 1);
    }
    return [...counts.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  }

  /** Reflect the current filter on the quick-filter buttons (they live in the
   *  header, outside the `#tree` that render() rebuilds). */
  function updateQuickFilters() {
    const onlyActive = hasToken("active:true");
    activeBtn.setAttribute("aria-pressed", onlyActive ? "true" : "false");
    activeBtn.classList.toggle("active", onlyActive);

    const tagToks = filterTokens(state.filter).filter((t) => t.startsWith("#"));
    tagsBtn.classList.toggle("active", tagToks.length > 0);
    tagsBtn.setAttribute(
      "title",
      tagToks.length
        ? `Filtrando por: ${tagToks.join(" ")}`
        : "Filtrar por etiqueta"
    );
  }

  function buildTagMenu() {
    tagMenu.innerHTML = "";
    const tags = usedTags();
    if (tags.length === 0) {
      tagMenu.appendChild(
        el("div", { class: "tag-menu-empty", text: "No hay etiquetas todavía." })
      );
      return;
    }
    const active = new Set(
      filterTokens(state.filter)
        .filter((t) => t.startsWith("#"))
        .map((t) => t.slice(1).toLowerCase())
    );
    for (const [tag, count] of tags) {
      const on = active.has(tag.toLowerCase());
      tagMenu.appendChild(
        el("button", {
          class: `tag-menu-item ${on ? "checked" : ""}`,
          role: "menuitemcheckbox",
          "aria-checked": on ? "true" : "false",
          onClick: (e) => {
            e.stopPropagation();
            toggleToken(`#${tag}`);
            buildTagMenu(); // refresh checks, keep the menu open for multi-select
          },
          html: `<span class="check">${on ? "✓" : ""}</span>
                 <span class="label">#${escapeHtml(tag)}</span>
                 <span class="count">${count}</span>`,
        })
      );
    }
  }

  function openTagMenu() {
    buildTagMenu();
    tagMenu.hidden = false;
    tagsBtn.setAttribute("aria-expanded", "true");
  }
  function closeTagMenu() {
    tagMenu.hidden = true;
    tagsBtn.setAttribute("aria-expanded", "false");
  }

  // ── Toolbar wiring ───────────────────────────────────────────────────────
  const filterInput = /** @type {HTMLInputElement} */ (
    document.getElementById("filter")
  );
  const clearBtn = document.getElementById("clear-filter");
  const activeBtn = document.getElementById("qf-active");
  const tagsBtn = document.getElementById("qf-tags");
  const tagMenu = document.getElementById("tag-menu");

  activeBtn.addEventListener("click", () => toggleToken("active:true"));
  tagsBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (tagMenu.hidden) openTagMenu();
    else closeTagMenu();
  });
  tagMenu.addEventListener("click", (e) => e.stopPropagation());
  document.addEventListener("click", () => {
    if (!tagMenu.hidden) closeTagMenu();
  });
  let filterDebounce = 0;
  filterInput.addEventListener("input", () => {
    window.clearTimeout(filterDebounce);
    const value = filterInput.value;
    filterDebounce = window.setTimeout(() => {
      state.filter = value;
      clearBtn.hidden = value.length === 0;
      updateQuickFilters();
      render();
      post("filterChanged", { value });
    }, 120);
  });
  clearBtn.addEventListener("click", () => applyFilter(""));

  document.querySelectorAll(".group-toggle button").forEach((btn) => {
    btn.addEventListener("click", () => {
      const g = btn.getAttribute("data-group");
      state.groupBy = g;
      updateGroupToggle();
      render();
      post("groupByChanged", { value: g });
    });
  });
  function updateGroupToggle() {
    document.querySelectorAll(".group-toggle button").forEach((btn) => {
      btn.setAttribute(
        "aria-checked",
        btn.getAttribute("data-group") === state.groupBy ? "true" : "false"
      );
    });
  }

  document
    .getElementById("action-new")
    .addEventListener("click", () => post("newSession"));
  const statsBtn = document.getElementById("action-stats");
  function updateStatsBtn() {
    statsBtn.setAttribute("aria-pressed", ui.showStats ? "true" : "false");
    statsBtn.classList.toggle("active", !!ui.showStats);
  }
  updateStatsBtn();
  statsBtn.addEventListener("click", () => {
    ui.showStats = !ui.showStats;
    vscode.setState(ui);
    updateStatsBtn();
    render();
  });
  // Collapse/expand every section in one shot. Toggles on current state:
  // if anything is open → collapse all; otherwise → expand all.
  function setAllCollapsed(collapse) {
    if (collapse) {
      for (const b of document.querySelectorAll(".bucket[data-key]")) {
        const k = b.getAttribute("data-key");
        if (k) ui.collapsed[k] = true;
      }
    } else {
      ui.collapsed = {};
    }
    vscode.setState(ui);
    render();
  }
  document.getElementById("action-collapse").addEventListener("click", () => {
    const anyExpanded = [
      ...document.querySelectorAll(".bucket[data-key]"),
    ].some((b) => b.getAttribute("aria-expanded") === "true");
    setAllCollapsed(anyExpanded);
  });
  document
    .getElementById("action-refresh")
    .addEventListener("click", () => post("refresh"));
  document
    .getElementById("action-import")
    .addEventListener("click", () => post("import"));
  document
    .getElementById("empty-refresh")
    .addEventListener("click", () => post("refresh"));

  // ── Receive state ────────────────────────────────────────────────────────
  window.addEventListener("message", (event) => {
    const msg = event.data;
    if (msg.type === "state") {
      state = { ...state, ...msg.state };
      filterInput.value = state.filter || "";
      clearBtn.hidden = !state.filter;
      updateGroupToggle();
      updateQuickFilters();
      if (!tagMenu.hidden) buildTagMenu();
      diag("state push", {
        providers: state.providers.length,
        sessions: state.sessions.length,
        active: state.sessions.filter((s) => s.isActive).length,
        groupBy: state.groupBy,
        filter: state.filter,
        showStats: !!ui.showStats,
      });
      render();
    }
  });

  // Expose minimal debug handles on `window` so the panel's state can be
  // inspected from the webview's DevTools without monkey-patching the IIFE.
  Object.assign(window.__aterm, {
    state: () => state,
    ui: () => ui,
    rerender: () => render(),
    resetUi: () => {
      ui = { collapsed: {}, showStats: false };
      vscode.setState(ui);
      render();
    },
  });

  // Ask for an initial state once the script is parsed.
  post("ready");
})();
} catch (e) {
  // Pop the error onto the page so it's visible even without DevTools.
  window.__aterm = window.__aterm || {};
  window.__aterm.error = e && (e.stack || e.message || String(e));
  console.error("[agentSessions] script falló:", e);
  try {
    const banner = document.createElement("div");
    banner.style.cssText =
      "padding:12px;background:#a00;color:#fff;font:13px monospace;white-space:pre-wrap;";
    banner.textContent =
      "Agent Sessions — error de carga:\n" + window.__aterm.error;
    document.body.insertBefore(banner, document.body.firstChild);
  } catch (_) {
    /* nothing we can do */
  }
}
