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
    sessionIcons: {},
    projectIcons: {},
    groups: {},
    sessionGroups: {},
    quotas: {},
    serviceStatus: {},
    activeKeys: [],
    groupBy: "provider",
    filter: "",
    home: "",
    costAlertDaily: 0,
    claudeContextWindow: "auto",
    // Webview-local (not pushed by the extension): full-text content search
    // results. When set ({query, hits}), the panel shows results instead of
    // the session tree.
    search: null,
  };
  /** Persisted UI-only state: collapsed buckets (by key), dashboard toggle. */
  let ui = vscode.getState() || { collapsed: {}, showStats: false };
  if (ui.showStats == null) ui.showStats = false;
  // Older persisted state may predate `collapsed`; every consumer indexes it.
  if (ui.collapsed == null || typeof ui.collapsed !== "object") ui.collapsed = {};
  if (ui.density !== "compact") ui.density = "comfortable";

  const NO_PROJECT = "(sin proyecto)";

  // ── Modo selección (multi-abrir / multi-borrar) ──────────────────────────
  // Not persisted: a transient mode the user opts into, picks cards, acts, exits.
  let selectMode = false;
  /** Set of `provider:id` keys currently ticked. */
  const selected = new Set();
  const sessionKey = (s) => `${s.provider}:${s.id}`;

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
    archive: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M1.5 2h13v3h-13V2Zm1 4h11v8h-11V6Zm3.5 2v1.5h4V8H6Z"/></svg>`,
    folderAdd: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M1.5 3a.5.5 0 0 1 .5-.5h4.41l1 1H14a.5.5 0 0 1 .5.5v3.1A4.5 4.5 0 0 0 8.26 13.5H3a1.5 1.5 0 0 1-1.5-1.5V3Z"/><path fill="currentColor" d="M12 8.5h1.2v1.8H15v1.2h-1.8V13H12v-1.5h-1.8v-1.2H12V8.5Z"/></svg>`,
    command: `<svg viewBox="0 0 16 16"><path fill="currentColor" d="M1.8 3.2h2v2h-2v-2Zm4 .3h8.4v1.4H5.8V3.5ZM1.8 7h2v2h-2V7Zm4 .3h8.4v1.4H5.8V7.3ZM1.8 10.8h2v2h-2v-2Zm4 .3h8.4v1.4H5.8v-1.4Z"/></svg>`,
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
  /** Context window for the % calc: Claude's is user-pinnable (the logs don't
   *  record it), other providers report it. */
  const effWindow = (s) => {
    if (s.provider === "claude") {
      if (state.claudeContextWindow === "200k") return 200000;
      if (state.claudeContextWindow === "1m") return 1000000;
    }
    return s.contextWindow;
  };

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
      const w = effWindow(s);
      if (s.contextTokens && w) return (s.contextTokens / w) * 100;
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
        if (value === "persisted" || value === "archived")
          return s.archivedOnly === true || !!(m && m.persisted);
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
      pruneSelection();
      doRender();
      updateSelectionBar();
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

  function clearSearch() {
    state.search = null;
    render();
  }

  /** Wrap case-insensitive matches of `query` in <mark>, escaping HTML per
   *  segment. Matching runs on the RAW text (not the escaped form) so neither
   *  the query nor the snippet's `& < > "` corrupt the output. */
  function highlight(text, query) {
    const src = text || "";
    if (!query) return escapeHtml(src);
    const q = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    let re;
    try {
      re = new RegExp(q, "gi");
    } catch (_) {
      return escapeHtml(src);
    }
    let out = "";
    let last = 0;
    let m;
    while ((m = re.exec(src)) !== null) {
      out += escapeHtml(src.slice(last, m.index));
      out += `<mark>${escapeHtml(m[0])}</mark>`;
      last = m.index + m[0].length;
      if (m.index === re.lastIndex) re.lastIndex++; // guard against zero-width matches
    }
    out += escapeHtml(src.slice(last));
    return out;
  }

  function searchHitCard(h, query) {
    const card = el("div", {
      class: "search-hit",
      title: "Reanudar esta sesión",
      onClick: () => post("resume", { provider: h.provider, id: h.id }),
    });
    const head = el("span", { class: "hit-head" });
    head.appendChild(
      el("span", {
        class: "avatar",
        style: {
          background:
            PROVIDER_AVATAR[h.provider] || "var(--vscode-charts-foreground)",
        },
        text: PROVIDER_INITIAL[h.provider] || "?",
      })
    );
    head.appendChild(
      el("span", { class: "hit-title", text: h.title || h.id.slice(0, 8) })
    );
    head.appendChild(el("span", { class: "hit-prov", text: h.provider }));
    card.appendChild(head);
    card.appendChild(
      el("div", { class: "snippet", html: highlight(h.snippet, query) })
    );
    return card;
  }

  /** Render content-search results in place of the session tree. */
  function renderSearchResults() {
    const { query, hits } = state.search;
    root.innerHTML = "";
    root.appendChild(
      el("div", {
        class: "filter-banner",
        onClick: () => clearSearch(),
        html: `<span>Contenido: <strong>${escapeHtml(query)}</strong></span>
               <span class="count">${hits.length} resultado(s) · clic para salir</span>`,
      })
    );
    if (hits.length === 0) {
      root.appendChild(
        el("p", {
          class: "hint",
          style: { padding: "12px 16px" },
          text: `Sin coincidencias para "${query}".`,
        })
      );
      return;
    }
    const list = el("div", { class: "cards" });
    for (const h of hits) list.appendChild(searchHitCard(h, query));
    root.appendChild(list);
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
    // Content-search results replace the tree until cleared.
    if (state.search) {
      root.hidden = false;
      emptyView.hidden = true;
      statsView.hidden = true;
      renderSearchResults();
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
    } else if (groupBy === "group") {
      renderGroupBuckets(filteredSessions);
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

  /** True if `a` is a strict ancestor directory of `b`. */
  function isAncestorPath(a, b) {
    if (a === b) return false;
    return b.startsWith(a + "/") || b.startsWith(a + "\\");
  }

  /** Project view, with sub-project nesting derived from the paths: a project
   *  whose cwd lives under another project's cwd is rendered indented beneath
   *  it (e.g. a monorepo with sessions in sub-packages). Projects with no
   *  ancestor among the others stay top-level; "(sin proyecto)" sinks to the
   *  bottom. */
  function renderProjectBuckets(sessions, /*root*/ _root) {
    const groups = bucketByProject(sessions);
    const itemsByCwd = new Map(groups);
    const order = groups.map((g) => g[0]); // recency order, preserved
    const real = order.filter((c) => c !== NO_PROJECT);

    // Nearest ancestor (longest matching prefix) among the projects with
    // sessions becomes the parent; the rest are roots.
    const childrenOf = new Map();
    const roots = [];
    for (const c of real) {
      let parent = null;
      for (const o of real) {
        if (isAncestorPath(o, c) && (!parent || o.length > parent.length))
          parent = o;
      }
      if (parent) {
        if (!childrenOf.has(parent)) childrenOf.set(parent, []);
        childrenOf.get(parent).push(c);
      } else {
        roots.push(c);
      }
    }

    const renderNode = (cwd, depth) => {
      const items = itemsByCwd.get(cwd) || [];
      const kids = childrenOf.get(cwd) || [];
      const key = `project:${cwd}`;
      const collapsed = !!ui.collapsed[key];
      // Total including descendants, shown when it differs from direct count.
      let total = items.length;
      const stack = [...kids];
      while (stack.length) {
        const k = stack.pop();
        total += (itemsByCwd.get(k) || []).length;
        for (const gk of childrenOf.get(k) || []) stack.push(gk);
      }
      root.appendChild(
        bucketHeader({
          key,
          label: depth === 0 ? projectLabel(cwd).toUpperCase() : projectLabel(cwd),
          count: items.length,
          subtotal: kids.length ? total : null,
          accentVar: projectAccentVar(cwd),
          cwd,
          collapsed,
          indent: depth,
          nested: depth > 0,
        })
      );
      if (collapsed) return;
      if (items.length) root.appendChild(cardList(items));
      for (const child of kids) renderNode(child, depth + 1);
    };

    for (const r of roots) renderNode(r, 0);

    // Sessions with no cwd last, flat.
    if (itemsByCwd.has(NO_PROJECT)) {
      const items = itemsByCwd.get(NO_PROJECT);
      const key = `project:${NO_PROJECT}`;
      const collapsed = !!ui.collapsed[key];
      root.appendChild(
        bucketHeader({ key, label: NO_PROJECT.toUpperCase(), count: items.length, collapsed })
      );
      if (!collapsed) root.appendChild(cardList(items));
    }
  }

  /** User-defined groups. Every defined group gets a bucket (even when empty,
   *  so it's visible and manageable); sessions without a group fall into a
   *  "(sin grupo)" bucket at the end. */
  function renderGroupBuckets(sessions) {
    const defs = state.groups || {};
    const assign = state.sessionGroups || {};
    const buckets = new Map();
    const ungrouped = [];
    for (const s of sessions) {
      const gid = assign[sessionKey(s)];
      if (gid && defs[gid]) {
        if (!buckets.has(gid)) buckets.set(gid, []);
        buckets.get(gid).push(s);
      } else {
        ungrouped.push(s);
      }
    }
    const ids = Object.keys(defs);
    if (ids.length === 0) {
      root.appendChild(
        el("p", {
          class: "hint",
          style: { padding: "12px 16px" },
          html:
            "Aún no tienes grupos. Crea uno desde el menú “⋯ → Mover a grupo…” " +
            "de una sesión, o con el comando <strong>Gestionar grupos…</strong>.",
        })
      );
    }
    for (const id of ids) {
      const items = buckets.get(id) || [];
      const key = `group:${id}`;
      const collapsed = !!ui.collapsed[key];
      root.appendChild(
        bucketHeader({
          key,
          label:
            (defs[id].icon ? defs[id].icon + " " : "") +
            (defs[id].name || id).toUpperCase(),
          count: items.length,
          accentVar: defs[id].color || "",
          collapsed,
          groupId: id,
        })
      );
      if (!collapsed && items.length) root.appendChild(cardList(items));
    }
    if (ungrouped.length) {
      const key = "group:__none__";
      const collapsed = !!ui.collapsed[key];
      root.appendChild(
        bucketHeader({
          key,
          label: "(SIN GRUPO)",
          count: ungrouped.length,
          collapsed,
          groupId: "__none__",
        })
      );
      if (!collapsed) root.appendChild(cardList(ungrouped));
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
    indent = 0,
    subtotal = null,
    groupId = null,
  }) {
    // Two kinds of drop target: project buckets (Claude move_session) and group
    // buckets (assign-to-group, any provider, including the "(sin grupo)" one).
    const isProjectDrop = cwd && cwd !== NO_PROJECT;
    const isGroupDrop = groupId != null;
    const isDropTarget = isProjectDrop || isGroupDrop;
    const node = el("div", {
      class: `bucket ${nested ? "nested" : ""} ${collapsed ? "collapsed" : ""}`,
      role: "treeitem",
      "aria-expanded": String(!collapsed),
      tabindex: "0",
      "data-key": key,
      style: indent > 0 ? { paddingLeft: `${10 + indent * 14}px` } : undefined,
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
              if (isGroupDrop) {
                post("assignGroup", {
                  items: [{ provider: data.provider, id: data.id }],
                  groupId,
                });
              } else if (
                data.provider === "claude" &&
                data.sourceCwd &&
                data.sourceCwd !== cwd
              ) {
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
    const projIcon =
      cwd && cwd !== NO_PROJECT ? state.projectIcons[cwd] : null;
    if (projIcon) {
      node.appendChild(el("span", { class: "bucket-icon", text: projIcon }));
    } else if (accentVar) {
      node.appendChild(
        el("span", { class: "swatch", style: { background: accentVar } })
      );
    }
    node.appendChild(el("span", { class: "name", text: label }));
    node.appendChild(
      el("span", {
        class: "meta",
        text:
          subtotal != null && subtotal !== count
            ? `${count} aquí · ${subtotal} con subproyectos`
            : count === 1
              ? "1 sesión"
              : `${count} sesiones`,
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
        actionBtn("Comandos del proyecto", ICONS.command, (e) => {
          e.stopPropagation();
          post("projectCommands", { cwd });
        })
      );
      actions.appendChild(
        actionBtn("Añadir carpeta al workspace", ICONS.folderAdd, (e) => {
          e.stopPropagation();
          post("addProjectToWorkspace", { cwd });
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
      actions.appendChild(
        actionBtn("Icono del proyecto", ICONS.star, (e) => {
          e.stopPropagation();
          post("setProjectIcon", { cwd });
        })
      );
      node.appendChild(actions);
    } else if (groupId && groupId !== "__none__") {
      // Inline action for a group bucket: manage groups (rename/color/delete).
      const actions = el("span", { class: "actions" });
      actions.appendChild(
        actionBtn("Gestionar grupos", ICONS.edit, (e) => {
          e.stopPropagation();
          post("manageGroups");
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
    const key = sessionKey(s);
    const inUse = state.activeKeys.includes(key);
    // Selectable in select mode unless it's an archived-only orphan (no live
    // jsonl to open/delete the usual way).
    const selectable = selectMode && !s.archivedOnly;
    const isSelected = selectable && selected.has(key);
    // Draggable for two drop targets: assigning to a group (any provider) and
    // moving between projects (Claude-only, enforced at drop). Archived-only
    // orphans and select mode opt out.
    const dragOk = !s.archivedOnly && !selectMode;
    const node = el("div", {
      class: `card ${inUse ? "in-use" : ""} ${isSelected ? "selected" : ""}`,
      role: "treeitem",
      tabindex: "0",
      "data-session-id": s.id,
      "data-provider": s.provider,
      draggable: dragOk ? "true" : "false",
      title: tooltipText(s, m, inUse),
      style: accent ? { "--card-accent": accent } : undefined,
      onClick: () => {
        if (selectable) {
          toggleSelect(key);
          return;
        }
        post("resume", { provider: s.provider, id: s.id });
      },
      onKeydown: (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          if (selectable) toggleSelect(key);
          else post("resume", { provider: s.provider, id: s.id });
        }
      },
      onContextmenu: (e) => {
        e.preventDefault();
        post("contextMenu", { provider: s.provider, id: s.id });
      },
      onDragstart: dragOk
        ? (e) => {
            const payload = JSON.stringify({
              provider: s.provider,
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

    // In select mode a checkbox takes the avatar's slot (same grid cell, so no
    // layout shift). Archived-only orphans aren't selectable → keep the avatar.
    if (selectable) {
      const box = el("span", { class: "select-box" });
      box.appendChild(
        el("input", {
          type: "checkbox",
          checked: isSelected ? "checked" : false,
          "aria-label": "Seleccionar sesión",
          onClick: (e) => {
            e.stopPropagation();
            toggleSelect(key);
          },
        })
      );
      node.appendChild(box);
    } else {
      // Avatar: a custom emoji icon if set, else the provider initial in a
      // coloured circle; green dot if live either way.
      const icon = state.sessionIcons[key];
      node.appendChild(
        el("span", {
          class: `avatar ${s.isActive ? "live" : ""} ${icon ? "emoji" : ""}`,
          style: icon
            ? undefined
            : {
                "--avatar-bg":
                  PROVIDER_AVATAR[s.provider] || "var(--vscode-charts-foreground)",
              },
          title: `${s.provider}`,
          text: icon || PROVIDER_INITIAL[s.provider] || s.provider[0].toUpperCase(),
        })
      );
    }

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
    const ctxWin = effWindow(s);
    if (s.contextTokens && ctxWin) {
      const pct = Math.round((s.contextTokens / ctxWin) * 100);
      let tone = "var(--vscode-charts-green)";
      if (pct >= 80) tone = "var(--vscode-charts-red)";
      else if (pct >= 60) tone = "var(--vscode-charts-orange)";
      else if (pct >= 40) tone = "var(--vscode-charts-yellow)";
      meta.appendChild(el("span", { class: "sep", text: "·" }));
      meta.appendChild(
        el("span", {
          class: "ctx-pct",
          style: { color: tone },
          title: `${s.contextTokens.toLocaleString()} / ${ctxWin.toLocaleString()} tokens`,
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
    // Archive indicator: blue box if a durable snapshot exists. For an
    // archived-only session (Claude deleted the original) it reads "restaurable".
    if (s.archivedOnly || (m && m.persisted)) {
      node.appendChild(
        el("span", {
          class: `archive-indicator ${s.archivedOnly ? "orphan" : ""}`,
          title: s.archivedOnly
            ? "Archivada (original borrado) — clic en la tarjeta para restaurar y reanudar"
            : "Persistida: copia durable (del estado al marcar) bajo ~/.config/aterm/archive",
          html: ICONS.archive,
        })
      );
    }

    // Hover actions (right side). Archived-only cards (original deleted) only
    // support resume-via-restore and the (reduced) "More…" menu — favourite
    // and preview would hit the missing original.
    const actions = el("span", { class: "actions" });
    if (!s.archivedOnly && !(m && m.favorite)) {
      actions.appendChild(
        actionBtn("Marcar favorito", ICONS.star, (e) => {
          e.stopPropagation();
          post("toggleFavorite", { provider: s.provider, id: s.id });
        })
      );
    }
    actions.appendChild(
      actionBtn(s.archivedOnly ? "Restaurar y reanudar" : "Reanudar", ICONS.play, (e) => {
        e.stopPropagation();
        post("resume", { provider: s.provider, id: s.id });
      })
    );
    if (!s.archivedOnly) {
      actions.appendChild(
        actionBtn("Previsualizar", ICONS.eye, (e) => {
          e.stopPropagation();
          post("preview", { provider: s.provider, id: s.id });
        })
      );
    }
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

  // ── Selección múltiple ─────────────────────────────────────────────────
  /** Sessions visible under the current filter that can be selected (i.e. not
   *  archived-only orphans). The "select all" target and the action payloads. */
  function visibleSelectable() {
    return state.sessions.filter(
      (s) => !s.archivedOnly && matchesFilter(s, state.filter)
    );
  }
  function toggleSelect(key) {
    if (selected.has(key)) selected.delete(key);
    else selected.add(key);
    render();
    updateSelectionBar();
  }
  function updateSelectionBar() {
    if (!selectionBar) return;
    selectionBar.hidden = !selectMode;
    const n = selected.size;
    selectionCount.textContent =
      n === 1 ? "1 seleccionada" : `${n} seleccionadas`;
    selectionOpen.disabled = n === 0;
    selectionDelete.disabled = n === 0;
    if (selectionGroup) selectionGroup.disabled = n === 0;
  }
  function setSelectMode(on) {
    selectMode = on;
    document.body.classList.toggle("selecting", on);
    if (!on) selected.clear();
    selectBtn.classList.toggle("active", on);
    selectBtn.setAttribute("aria-pressed", on ? "true" : "false");
    render();
    updateSelectionBar();
  }
  /** Prune keys that no longer match (filter changed / session vanished). */
  function pruneSelection() {
    if (!selectMode || selected.size === 0) return;
    const visible = new Set(visibleSelectable().map(sessionKey));
    for (const k of [...selected]) if (!visible.has(k)) selected.delete(k);
  }

  // ── Toolbar wiring ───────────────────────────────────────────────────────
  const filterInput = /** @type {HTMLInputElement} */ (
    document.getElementById("filter")
  );
  const selectBtn = document.getElementById("action-select");
  const selectionBar = document.getElementById("selection-bar");
  const selectionCount = document.getElementById("selection-count");
  const selectionOpen = document.getElementById("selection-open");
  const selectionGroup = document.getElementById("selection-group");
  const selectionDelete = document.getElementById("selection-delete");
  const selectionCancel = document.getElementById("selection-cancel");
  const selectionAll = document.getElementById("selection-all");

  selectBtn.addEventListener("click", () => setSelectMode(!selectMode));
  selectionCancel.addEventListener("click", () => setSelectMode(false));
  selectionAll.addEventListener("click", () => {
    const all = visibleSelectable().map(sessionKey);
    // Toggle: if everything visible is already ticked, clear; else select all.
    const allTicked = all.length > 0 && all.every((k) => selected.has(k));
    selected.clear();
    if (!allTicked) for (const k of all) selected.add(k);
    render();
    updateSelectionBar();
  });
  const keysToPayload = () =>
    [...selected].map((k) => {
      const i = k.indexOf(":");
      return { provider: k.slice(0, i), id: k.slice(i + 1) };
    });
  selectionOpen.addEventListener("click", () => {
    if (selected.size === 0) return;
    post("openMany", { items: keysToPayload() });
    setSelectMode(false);
  });
  selectionGroup.addEventListener("click", () => {
    if (selected.size === 0) return;
    // The extension shows the group picker and exits select mode afterwards.
    post("assignGroup", { items: keysToPayload() });
  });
  selectionDelete.addEventListener("click", () => {
    if (selected.size === 0) return;
    // The extension shows the confirm modal; it calls back to exit on success.
    post("deleteMany", { items: keysToPayload() });
  });
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
      // Typing a normal filter exits content-search mode.
      state.search = null;
      updateQuickFilters();
      render();
      post("filterChanged", { value });
    }, 120);
  });
  clearBtn.addEventListener("click", () => applyFilter(""));

  // Full-text content search: uses whatever is in the filter box as the query
  // and renders hits in-panel (see renderSearchResults).
  document.getElementById("action-fts").addEventListener("click", () => {
    const q = filterInput.value.trim();
    if (!q) {
      filterInput.focus();
      return;
    }
    post("searchContent", { query: q });
  });

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
    if (ui.showStats) state.search = null; // leave content-search when opening stats
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

  // Density toggle: comfortable ↔ compact (tighter cards, still two lines). Persisted.
  function applyDensity() {
    document.body.classList.toggle("compact", ui.density === "compact");
  }
  const densityBtn = document.getElementById("action-density");
  function updateDensityBtn() {
    densityBtn.classList.toggle("active", ui.density === "compact");
    densityBtn.setAttribute("aria-pressed", ui.density === "compact" ? "true" : "false");
  }
  densityBtn.addEventListener("click", () => {
    ui.density = ui.density === "compact" ? "comfortable" : "compact";
    vscode.setState(ui);
    applyDensity();
    updateDensityBtn();
  });
  applyDensity();
  updateDensityBtn();
  document
    .getElementById("action-refresh")
    .addEventListener("click", () => post("refresh"));
  document
    .getElementById("action-more")
    .addEventListener("click", () => post("actionsMenu"));
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
    } else if (msg.type === "exitSelect") {
      // The extension finished a multi-delete; drop out of select mode.
      if (selectMode) setSelectMode(false);
    } else if (msg.type === "searchResults") {
      // Search and stats are mutually-exclusive top views.
      if (ui.showStats) {
        ui.showStats = false;
        vscode.setState(ui);
        updateStatsBtn();
      }
      state.search = { query: msg.query, hits: msg.hits || [] };
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
      ui = { collapsed: {}, showStats: false, density: "comfortable" };
      vscode.setState(ui);
      applyDensity();
      updateDensityBtn();
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
