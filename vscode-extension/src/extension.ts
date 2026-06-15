// Agent Sessions — VS Code extension.
//
// A webview-view UI over the `agent-sessions-cli` sidecar (shared Rust core
// with the native `aterm` app). The heavy lifting — discovering / parsing
// sessions, plus metadata, projects and export/import on disk — lives in the
// sidecar; this extension renders an HTML/CSS panel, drives the integrated
// terminal and prompts via VS Code dialogs.
//
// We moved off TreeDataProvider so each session row can be a full-height
// "card" (avatar + two lines + tags + colour-coded project accent), which
// TreeView doesn't allow (all rows must share the workbench list height).

import * as cp from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";

let extensionPath = "";

/** Output channel where the webview pipes diagnostics. Visible via
 *  `View → Output → Agent Sessions`. Avoids relying on DevTools, which
 *  doesn't always pop as a separate window. */
let output: vscode.OutputChannel | null = null;
function log(line: string): void {
  if (!output) return;
  const ts = new Date().toLocaleTimeString();
  output.appendLine(`[${ts}] ${line}`);
}

interface Session {
  provider: string;
  id: string;
  title: string | null;
  cwd: string | null;
  branch: string | null;
  messageCount: number | null;
  lastActivity: number;
  isActive: boolean;
  /** "busy" | "idle" | other ("shell", …). Only Claude reports it today. */
  liveStatus: string | null;
  model: string | null;
  contextTokens: number | null;
  contextWindow: number | null;
  costUsd: number | null;
  resumeArgv: string[];
}

interface ProviderInfo {
  id: string;
  displayName: string;
  available: boolean;
  binaryFound: boolean;
  newSessionArgv: string[];
}

interface ServiceStatus {
  provider: string;
  /** "none" | "minor" | "major" | "critical" | "unknown" (statuspage.io). */
  indicator: string;
  description: string;
}

interface LiveAgentSession {
  provider: string;
  sessionId: string;
  pid: number;
  /** "busy" | "idle" | other ("shell", custom strings). */
  status: string | null;
}

interface QuotaWindow {
  label: string;
  usedPercent: number;
  resetsAt: number | null;
}

interface ProviderQuota {
  provider: string;
  windows: QuotaWindow[];
  asOf: number | null;
}

interface ScanResult {
  providers: ProviderInfo[];
  sessions: Session[];
  quotas?: Record<string, ProviderQuota>;
}

interface PreviewTurn {
  role: string;
  text: string;
}

interface SessionMetadata {
  name?: string | null;
  tags?: string[] | null;
  color?: string | null;
  notes?: string | null;
  favorite?: boolean;
}

interface ImportOutcome {
  imported: { id: string; displayName: string | null }[];
  skippedExisting: string[];
  skippedMissing: string[];
}

interface ProjectStore {
  names: Record<string, string>;
  colors: Record<string, string>;
}

interface ProjectPatch {
  name?: string | null;
  color?: string | null;
}

type GroupMode = "provider" | "project" | "cascade";

// ── Sidecar invocation ─────────────────────────────────────────────────────

function cliPath(): string {
  const configured = vscode.workspace
    .getConfiguration("agentSessions")
    .get<string>("cliPath", "agent-sessions-cli");
  if (configured && configured !== "agent-sessions-cli") {
    return configured;
  }
  if (extensionPath) {
    const exe = process.platform === "win32" ? ".exe" : "";
    const candidates = [
      path.join(extensionPath, "bin", platformTarget(), `agent-sessions-cli${exe}`),
      path.join(extensionPath, "..", "target", "release", `agent-sessions-cli${exe}`),
      path.join(extensionPath, "..", "target", "debug", `agent-sessions-cli${exe}`),
    ];
    for (const c of candidates) {
      if (fs.existsSync(c)) return c;
    }
  }
  return "agent-sessions-cli";
}

function platformTarget(): string {
  const arch =
    process.arch === "x64" ? "x86_64" : process.arch === "arm64" ? "aarch64" : process.arch;
  switch (process.platform) {
    case "linux":
      return `${arch}-unknown-linux-gnu`;
    case "darwin":
      return `${arch}-apple-darwin`;
    case "win32":
      return `${arch}-pc-windows-msvc`;
    default:
      return `${arch}-${process.platform}`;
  }
}

function runCli<T>(args: string[], stdin?: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const child = cp.execFile(
      cliPath(),
      args,
      { maxBuffer: 32 * 1024 * 1024 },
      (err, stdout, stderr) => {
        if (err) {
          reject(new Error(stderr.trim() || err.message));
          return;
        }
        const out = stdout.trim();
        if (out === "") {
          resolve(undefined as unknown as T);
          return;
        }
        try {
          resolve(JSON.parse(out) as T);
        } catch (e) {
          reject(new Error(`salida no-JSON del sidecar: ${e}`));
        }
      }
    );
    if (stdin !== undefined && child.stdin) {
      child.stdin.end(stdin);
    }
  });
}

// ── Webview state ──────────────────────────────────────────────────────────

class SessionsView implements vscode.WebviewViewProvider {
  static readonly viewType = "agentSessions.sessions";

  private view: vscode.WebviewView | null = null;
  private scan: ScanResult = { providers: [], sessions: [] };
  private metadata: Record<string, SessionMetadata> = {};
  private projects: ProjectStore = { names: {}, colors: {} };
  /** Statuspage health per provider id. Refreshed independently from scan
   *  so the (network-bound) curl doesn't slow down session discovery. */
  private serviceStatus: Record<string, ServiceStatus> = {};
  /** UI filter mirrored from the webview; persisted only in the webview's
   *  retained state, but we cache it here so command-palette commands can read
   *  the current value. */
  private filter: string = "";
  /** `provider:id` → terminal we launched for that session. Lets us focus the
   *  existing terminal instead of double-resuming (which would corrupt the
   *  transcript: two CLI processes writing the same jsonl). */
  private activeTerminals = new Map<string, vscode.Terminal>();
  /** Last observed live status per `provider:id`, for transition detection
   *  (busy→idle = needs input, alive→gone = finished). */
  private prevLive = new Map<string, string>();
  private pollTimer: NodeJS.Timeout | null = null;
  /** Permanent status-bar widget. Shows live counts + today's spend so the
   *  user keeps an eye on Agent Sessions even when the panel is hidden. */
  private statusItem: vscode.StatusBarItem | null = null;

  constructor(private readonly context: vscode.ExtensionContext) {
    // Status-bar widget: always visible, clicking it reveals the panel.
    this.statusItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Right,
      100
    );
    this.statusItem.command = "agentSessions.focus";
    this.statusItem.text = "$(comment-discussion) Agent Sessions";
    this.statusItem.tooltip = "Abrir panel de sesiones de agentes";
    this.statusItem.show();
    context.subscriptions.push(this.statusItem);

    // Watch for terminals closing externally so the "session in use" badge
    // disappears when the user kills the agent themselves.
    context.subscriptions.push(
      vscode.window.onDidCloseTerminal((closed) => {
        for (const [key, t] of this.activeTerminals) {
          if (t === closed) {
            this.activeTerminals.delete(key);
            this.push();
            break;
          }
        }
      })
    );
    // Recurring live-status poll. Re-armed when settings change.
    this.armPoll();
    context.subscriptions.push(
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("agentSessions.pollIntervalSec")) {
          this.armPoll();
        }
      })
    );
    // Stop the timer if the extension goes away (host shutdown).
    context.subscriptions.push({
      dispose: () => {
        if (this.pollTimer) clearInterval(this.pollTimer);
      },
    });
  }

  /** (Re)start the polling timer with the configured interval. */
  private armPoll(): void {
    if (this.pollTimer) clearInterval(this.pollTimer);
    const sec = Math.max(
      2,
      vscode.workspace
        .getConfiguration("agentSessions")
        .get<number>("pollIntervalSec", 5)
    );
    this.pollTimer = setInterval(() => void this.pollLive(), sec * 1000);
  }

  /** Cheap live-registry poll. Updates the cached scan's `isActive` /
   *  `liveStatus` fields without re-reading every transcript, then diffs
   *  against the previous state to emit user-visible notifications. */
  private async pollLive(): Promise<void> {
    let live: LiveAgentSession[];
    try {
      live = (await runCli<LiveAgentSession[]>(["live"])) || [];
    } catch {
      return;
    }
    const byKey = new Map<string, LiveAgentSession>();
    for (const l of live) byKey.set(`${l.provider}:${l.sessionId}`, l);

    // Update the cached sessions' isActive / liveStatus in place — that's
    // what the webview reads for the per-provider state counters.
    let mutated = false;
    for (const s of this.scan.sessions) {
      const l = byKey.get(`${s.provider}:${s.id}`);
      const nowActive = !!l;
      const nowStatus = l?.status ?? null;
      if (s.isActive !== nowActive || s.liveStatus !== nowStatus) {
        s.isActive = nowActive;
        s.liveStatus = nowStatus;
        mutated = true;
      }
    }
    if (mutated) this.push();

    // Diff against previous state for notifications.
    const cfg = vscode.workspace.getConfiguration("agentSessions");
    const notifyIdle = cfg.get<boolean>("notifyOnIdle", true);
    const notifyFinish = cfg.get<boolean>("notifyOnFinish", true);
    const seen = new Set<string>();
    for (const [key, l] of byKey) {
      seen.add(key);
      const prev = this.prevLive.get(key);
      // busy → idle = the agent is waiting for the user.
      if (notifyIdle && prev === "busy" && l.status === "idle") {
        notifySession(this, l, "Esperando tu input");
      }
      // (other transitions could be added: idle → busy is the user replying,
      // not worth a notification.)
    }
    if (notifyFinish) {
      for (const [key] of this.prevLive) {
        if (!seen.has(key)) {
          // Session was alive last tick, gone now → conversation ended.
          const [provider, id] = key.split(":", 2);
          notifySession(
            this,
            { provider, sessionId: id, pid: 0, status: null },
            "Conversación terminada"
          );
        }
      }
    }
    // Update the rolling state.
    this.prevLive = new Map();
    for (const [key, l] of byKey) {
      if (l.status) this.prevLive.set(key, l.status);
      else this.prevLive.set(key, "alive");
    }
  }

  /** Stable identity for a (provider, id) pair. */
  static keyOf(provider: string, id: string): string {
    return `${provider}:${id}`;
  }

  /** Recompute the status-bar text from the current scan. Cheap; called on
   *  every push. Format: `$(icon) N activas · $X.XX hoy`. The icon (and
   *  background colour) reacts to live state — orange if anything is busy,
   *  red if the daily-cost alert tripped. */
  private updateStatusBar(): void {
    if (!this.statusItem) return;
    let active = 0;
    let busy = 0;
    let idle = 0;
    for (const s of this.scan.sessions) {
      if (!s.isActive) continue;
      active++;
      if (s.liveStatus === "busy") busy++;
      else if (s.liveStatus === "idle") idle++;
    }
    // Today's cost.
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const since = today.getTime() / 1000;
    let cost = 0;
    for (const s of this.scan.sessions) {
      if (s.lastActivity >= since && s.costUsd) cost += s.costUsd;
    }
    const cfg = vscode.workspace.getConfiguration("agentSessions");
    const alert = cfg.get<number>("costAlertDaily", 0) || 0;

    // Icon reflects most-urgent live state.
    let icon = "$(comment-discussion)";
    if (idle > 0) icon = "$(bell-dot)"; // waiting on user
    else if (busy > 0) icon = "$(sync~spin)";

    const parts: string[] = [];
    if (active > 0) {
      const bits: string[] = [];
      if (busy) bits.push(`${busy}⚡`);
      if (idle) bits.push(`${idle}⏳`);
      const other = active - busy - idle;
      if (other) bits.push(`${other}●`);
      parts.push(bits.join(" "));
    } else {
      parts.push("Agent Sessions");
    }
    if (cost > 0) parts.push(`$${cost.toFixed(2)}`);

    this.statusItem.text = `${icon} ${parts.join(" · ")}`;
    this.statusItem.tooltip = new vscode.MarkdownString(
      [
        `**Agent Sessions**`,
        active === 1 ? `1 sesión activa` : `${active} sesiones activas`,
        busy ? `${busy} trabajando` : "",
        idle ? `${idle} esperando input` : "",
        `Hoy: $${cost.toFixed(2)}`,
        alert > 0 ? `Umbral: $${alert.toFixed(2)}` : "",
      ]
        .filter(Boolean)
        .join("\n\n")
    );
    if (alert > 0 && cost >= alert) {
      this.statusItem.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.errorBackground"
      );
    } else if (idle > 0) {
      this.statusItem.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.warningBackground"
      );
    } else {
      this.statusItem.backgroundColor = undefined;
    }
  }

  /** True if we already opened a terminal for this session in this VS Code
   *  window. */
  hasActiveTerminal(provider: string, id: string): boolean {
    return this.activeTerminals.has(SessionsView.keyOf(provider, id));
  }

  /** Focus the existing terminal for this session, if any. Returns true when
   *  it found one; the caller falls through to launching otherwise. */
  focusActiveTerminal(provider: string, id: string): boolean {
    const t = this.activeTerminals.get(SessionsView.keyOf(provider, id));
    if (!t) return false;
    t.show(true);
    return true;
  }

  /** Remember the terminal a resume launched, so later clicks focus instead
   *  of re-launching. */
  registerTerminal(provider: string, id: string, terminal: vscode.Terminal): void {
    this.activeTerminals.set(SessionsView.keyOf(provider, id), terminal);
    this.push();
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    const mediaRoot = vscode.Uri.joinPath(
      this.context.extensionUri,
      "media",
      "webview"
    );
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [mediaRoot],
    };
    view.webview.html = this.renderHtml(view.webview, mediaRoot);

    const sub = view.webview.onDidReceiveMessage((m) => this.handleMessage(m));
    const disposeSub = view.onDidDispose(() => {
      sub.dispose();
      disposeSub.dispose();
      if (this.view === view) this.view = null;
    });
    void this.refresh();
  }

  /** Re-scan the sidecar and push the new state to the webview. */
  async refresh(): Promise<void> {
    try {
      const [scan, meta, projects] = await Promise.all([
        runCli<ScanResult>(["scan"]),
        runCli<Record<string, SessionMetadata>>(["metadata-get"]).catch(() => ({})),
        runCli<ProjectStore>(["projects-get"]).catch(() => ({ names: {}, colors: {} })),
      ]);
      this.scan = scan;
      this.metadata = meta ?? {};
      this.projects = projects ?? { names: {}, colors: {} };
    } catch (e) {
      this.scan = { providers: [], sessions: [] };
      vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    }
    this.push();
    // Statuspage check is network-bound: fire and forget so the panel
    // renders immediately, then re-push when the status arrives.
    void this.refreshServiceStatus();
  }

  /** Pull statuspage health for the providers that publish one. Pushes a new
   *  state to the webview when done. Errors are swallowed (no badge then). */
  private async refreshServiceStatus(): Promise<void> {
    try {
      const list = await runCli<ServiceStatus[]>(["service-status"]);
      const next: Record<string, ServiceStatus> = {};
      for (const s of list || []) next[s.provider] = s;
      this.serviceStatus = next;
      this.push();
    } catch {
      /* leave previous status in place */
    }
  }

  /** Push current state to the webview without re-scanning. */
  push(): void {
    this.updateStatusBar();
    if (!this.view) return;
    const cfg = vscode.workspace.getConfiguration("agentSessions");
    this.view.webview.postMessage({
      type: "state",
      state: {
        providers: this.scan.providers,
        sessions: this.scan.sessions,
        metadata: this.metadata,
        projects: this.projects,
        quotas: this.scan.quotas || {},
        serviceStatus: this.serviceStatus,
        activeKeys: Array.from(this.activeTerminals.keys()),
        groupBy: currentGroupMode(),
        filter: this.filter,
        home: os.homedir(),
        costAlertDaily: cfg.get<number>("costAlertDaily", 0) || 0,
      },
    });
  }

  applyMetadata(provider: string, id: string, entry: SessionMetadata | null): void {
    const key = `${provider}:${id}`;
    if (
      entry &&
      (entry.name ||
        (entry.tags && entry.tags.length) ||
        entry.color ||
        entry.notes ||
        entry.favorite)
    ) {
      this.metadata[key] = entry;
    } else {
      delete this.metadata[key];
    }
    this.push();
  }

  applyProject(cwd: string, entry: ProjectPatch | null): void {
    if (!entry || (!entry.name && !entry.color)) {
      delete this.projects.names[cwd];
      delete this.projects.colors[cwd];
    } else {
      if (entry.name) this.projects.names[cwd] = entry.name;
      else delete this.projects.names[cwd];
      if (entry.color) this.projects.colors[cwd] = entry.color;
      else delete this.projects.colors[cwd];
    }
    this.push();
  }

  metadataFor(provider: string, id: string): SessionMetadata | null {
    return this.metadata[`${provider}:${id}`] ?? null;
  }
  projectAliasFor(cwd: string): string | null {
    return this.projects.names[cwd] ?? null;
  }
  sessionsSnapshot(): Session[] {
    return this.scan.sessions;
  }
  currentFilter(): string {
    return this.filter;
  }

  /** Route messages from the webview to extension command handlers. */
  private async handleMessage(msg: any): Promise<void> {
    switch (msg.command) {
      case "ready":
        log("webview: ready");
        this.push();
        return;
      case "diag":
        log(
          `webview: ${msg.label}: ${
            msg.data === undefined
              ? ""
              : typeof msg.data === "string"
                ? msg.data
                : JSON.stringify(msg.data)
          }`
        );
        return;
      case "refresh":
        return this.refresh();
      case "filterChanged":
        this.filter = String(msg.value || "");
        return;
      case "groupByChanged":
        await vscode.workspace
          .getConfiguration("agentSessions")
          .update("groupBy", msg.value, vscode.ConfigurationTarget.Global);
        return;
      case "setFilter":
        this.filter = String(msg.value || "");
        this.push();
        return;
      case "newSession":
        return newSession();
      case "import":
        return importArchive(this);
      case "resume": {
        const s = this.findSession(msg.provider, msg.id);
        if (s) await resumeSession(this, s, this.metadataFor(s.provider, s.id));
        return;
      }
      case "preview": {
        const s = this.findSession(msg.provider, msg.id);
        if (s) await preview(s);
        return;
      }
      case "contextMenu": {
        const s = this.findSession(msg.provider, msg.id);
        if (s) await sessionContextMenu(this, s);
        return;
      }
      case "renameProject":
        return renameProject(this, { cwd: msg.cwd });
      case "setProjectColor":
        return setProjectColor(this, { cwd: msg.cwd });
      case "moveSession":
        return moveSessionToProject(this, msg.id, msg.sourceCwd, msg.destCwd);
      case "toggleFavorite": {
        const s = this.findSession(msg.provider, msg.id);
        if (s) await toggleFavorite(this, s);
        return;
      }
    }
  }

  private findSession(provider: string, id: string): Session | undefined {
    return this.scan.sessions.find((s) => s.provider === provider && s.id === id);
  }

  private renderHtml(webview: vscode.Webview, mediaRoot: vscode.Uri): string {
    const indexPath = vscode.Uri.joinPath(mediaRoot, "index.html");
    const stylePath = vscode.Uri.joinPath(mediaRoot, "main.css");
    const scriptPath = vscode.Uri.joinPath(mediaRoot, "main.js");

    let html = fs.readFileSync(indexPath.fsPath, "utf8");

    const nonce = crypto();
    // `'unsafe-inline'` for style-src is intentional and recommended by VS
    // Code: the workbench injects an inline `<style>` block into every
    // webview iframe that defines the theme's `--vscode-*` CSS variables.
    // Blocking inline styles strips out the entire theme, leaving the page
    // with browser-default colours (white background, light-grey text) —
    // looks empty even though the DOM is populated correctly.
    const csp = [
      `default-src 'none'`,
      `style-src ${webview.cspSource} 'unsafe-inline'`,
      `script-src 'nonce-${nonce}' ${webview.cspSource}`,
      `font-src ${webview.cspSource}`,
      `img-src ${webview.cspSource} data:`,
    ].join("; ");

    html = html
      .replace("__CSP__", csp)
      .replace("__STYLE__", webview.asWebviewUri(stylePath).toString())
      .replace(
        '<script src="__SCRIPT__"></script>',
        `<script nonce="${nonce}" src="${webview.asWebviewUri(scriptPath)}"></script>`
      );
    return html;
  }
}

/** A 16-byte random nonce for the CSP. Webviews require one to allow inline-
 *  attributed script tags. */
function crypto(): string {
  let s = "";
  const chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
  for (let i = 0; i < 32; i++) s += chars[Math.floor(Math.random() * chars.length)];
  return s;
}

/** Small toast for a live-state transition. Title carries the session
 *  display name (or a short id) so the user knows *which* session paged. */
function notifySession(
  view: SessionsView,
  l: LiveAgentSession,
  reason: string
): void {
  const s = (view as any).scan.sessions.find(
    (x: Session) => x.provider === l.provider && x.id === l.sessionId
  ) as Session | undefined;
  const meta = s ? view.metadataFor(l.provider, l.sessionId) : null;
  const label =
    meta?.name?.trim() ||
    s?.title?.trim() ||
    `${l.provider} ${l.sessionId.slice(0, 8)}`;
  const action = view.hasActiveTerminal(l.provider, l.sessionId)
    ? "Abrir terminal"
    : "Reanudar";
  void vscode.window
    .showInformationMessage(`${label} — ${reason}.`, action)
    .then((pick) => {
      if (pick !== action) return;
      if (view.focusActiveTerminal(l.provider, l.sessionId)) return;
      if (s) void resumeSession(view, s, meta);
    });
}

function currentGroupMode(): GroupMode {
  const raw = vscode.workspace
    .getConfiguration("agentSessions")
    .get<string>("groupBy", "provider");
  return raw === "project" || raw === "cascade" ? raw : "provider";
}

// ── Terminal launch ─────────────────────────────────────────────────────────

function shellJoin(argv: string[]): string {
  return argv
    .map((a) => (/^[\w./:=-]+$/.test(a) ? a : `'${a.replace(/'/g, `'\\''`)}'`))
    .join(" ");
}

function launch(
  name: string,
  cwd: string | null | undefined,
  argv: string[]
): vscode.Terminal | null {
  if (argv.length === 0) {
    vscode.window.showWarningMessage(
      "Agent Sessions: no hay comando para esta acción (¿binario del proveedor en PATH?)."
    );
    return null;
  }
  const inEditor = vscode.workspace
    .getConfiguration("agentSessions")
    .get<boolean>("openInEditor", true);
  const terminal = vscode.window.createTerminal({
    name,
    cwd: cwd ?? undefined,
    location: inEditor ? vscode.TerminalLocation.Editor : undefined,
  });
  terminal.show();
  const closeOnExit = vscode.workspace
    .getConfiguration("agentSessions")
    .get<boolean>("closeOnExit", true);
  const line = closeOnExit ? `${shellJoin(argv)}; exit` : shellJoin(argv);
  terminal.sendText(line, true);
  return terminal;
}

/** Resume in a terminal, but if we already opened one for this session in
 *  this VS Code window, just bring it to the front — double-resuming would
 *  put two agent processes on the same on-disk transcript and corrupt it. */
async function resumeSession(
  view: SessionsView,
  s: Session,
  meta: SessionMetadata | null
): Promise<void> {
  if (view.focusActiveTerminal(s.provider, s.id)) return;
  const name = (meta?.name?.trim() || s.title?.trim() || s.provider).slice(0, 30);
  const terminal = launch(`▶ ${name}`, s.cwd, s.resumeArgv);
  if (terminal) view.registerTerminal(s.provider, s.id, terminal);
}

async function preview(s: Session): Promise<void> {
  try {
    const turns = await runCli<PreviewTurn[]>(["preview", s.provider, s.id]);
    const md = turns
      .map(
        (t) => `### ${t.role === "user" ? "🧑 Usuario" : "🤖 Asistente"}\n\n${t.text}`
      )
      .join("\n\n---\n\n");
    const doc = await vscode.workspace.openTextDocument({
      content: `# ${s.title ?? s.id}\n\n${md}`,
      language: "markdown",
    });
    await vscode.window.showTextDocument(doc, { preview: true });
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: previsualización no disponible (${(e as Error).message}).`
    );
  }
}

// ── Parallel orchestration (git worktrees) ──────────────────────────────────

/** Promisified execFile that swallows the stdout/stderr split: we only need
 *  to know if the command succeeded and what it said on error. */
function exec(file: string, args: string[], cwd?: string): Promise<string> {
  return new Promise((resolve, reject) => {
    cp.execFile(file, args, { cwd, maxBuffer: 16 * 1024 * 1024 }, (err, stdout, stderr) => {
      if (err) reject(new Error(stderr.trim() || err.message));
      else resolve(stdout);
    });
  });
}

/** Launch the same prompt with several agents in parallel, each in its own
 *  git worktree so they don't stomp on each other. The user picks which
 *  agents to use and types the prompt; we create the worktrees, open a
 *  terminal per agent, fire up the CLI and (best-effort) paste the prompt. */
async function launchParallel(): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    vscode.window.showWarningMessage(
      "Agent Sessions: abre una carpeta antes de lanzar una comparativa."
    );
    return;
  }
  const repoRoot = folder.uri.fsPath;
  try {
    await exec("git", ["rev-parse", "--show-toplevel"], repoRoot);
  } catch {
    vscode.window.showWarningMessage(
      "Agent Sessions: la carpeta abierta no es un repo git (necesario para worktrees)."
    );
    return;
  }

  let providers: ProviderInfo[];
  try {
    providers = await runCli<ProviderInfo[]>(["providers"]);
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  const usable = providers.filter((p) => p.binaryFound);
  if (usable.length < 2) {
    vscode.window.showWarningMessage(
      "Agent Sessions: necesitas al menos 2 agentes en PATH para una comparativa."
    );
    return;
  }

  const picks = await vscode.window.showQuickPick(
    usable.map((p) => ({ label: p.displayName, picked: true, info: p })),
    {
      canPickMany: true,
      placeHolder: "Agentes para la comparativa (espacio para alternar, Enter para confirmar)",
    }
  );
  if (!picks || picks.length === 0) return;

  const prompt = await vscode.window.showInputBox({
    title: "Prompt inicial (opcional)",
    prompt: "Se intentará pegar en cada terminal tras 2 s. Vacío = solo lanza el shell.",
    placeHolder: "p. ej. Refactoriza term/mod.rs para extraer la lógica de selección",
  });
  // showInputBox returns undefined on cancel — abort. Empty string is ok.
  if (prompt === undefined) return;

  // Worktrees live next to the repo so VS Code can open them as folders.
  // Stamp the dir + branch with a short timestamp so re-running doesn't clash.
  const stamp = Date.now().toString(36);
  const parent = path.dirname(repoRoot);
  const repoName = path.basename(repoRoot);

  const launched: string[] = [];
  for (const pick of picks) {
    const id = pick.info.id;
    const worktreePath = path.join(parent, `${repoName}-${id}-${stamp}`);
    const branch = `agents/${id}-${stamp}`;
    try {
      await exec(
        "git",
        ["worktree", "add", "-B", branch, worktreePath, "HEAD"],
        repoRoot
      );
    } catch (e) {
      vscode.window.showWarningMessage(
        `Agent Sessions: no se pudo crear worktree para ${pick.info.displayName} (${(e as Error).message}).`
      );
      continue;
    }

    const terminal = vscode.window.createTerminal({
      name: `⚡ ${pick.info.displayName}`,
      cwd: worktreePath,
      location: vscode.TerminalLocation.Editor,
    });
    terminal.show();
    terminal.sendText(shellJoin(pick.info.newSessionArgv), true);
    if (prompt.trim()) {
      // Some TUIs (claude, codex) need a moment to render their input area
      // before they'll accept text. A short delay is a pragmatic best-effort.
      setTimeout(() => terminal.sendText(prompt, false), 2500);
    }
    launched.push(branch);
  }

  if (launched.length === 0) return;
  vscode.window.showInformationMessage(
    `Agent Sessions: lanzados ${launched.length} agentes en worktrees bajo ${parent}. ` +
      `Branches: ${launched.join(", ")}. Para limpiar: "Limpiar worktrees…" en la paleta.`
  );
}

/** Open a Markdown report comparing the changes each comparison-worktree
 *  produced: a header per agent with `git diff --stat HEAD`, the commits the
 *  branch has on top of HEAD, and a link to open the worktree as a folder.
 *  Run after a `launchParallel` session to see at a glance who did what. */
async function compareWorktrees(): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    vscode.window.showWarningMessage(
      "Agent Sessions: abre primero la carpeta del repo."
    );
    return;
  }
  const repoRoot = folder.uri.fsPath;
  let raw: string;
  try {
    raw = await exec("git", ["worktree", "list", "--porcelain"], repoRoot);
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  const trees: { path: string; branch: string }[] = [];
  let cur: { path?: string; branch?: string } = {};
  for (const line of raw.split("\n")) {
    if (line.startsWith("worktree ")) cur = { path: line.slice(9) };
    else if (line.startsWith("branch ")) {
      cur.branch = line.slice(7).replace(/^refs\/heads\//, "");
    } else if (line.trim() === "" && cur.path) {
      if (cur.branch && cur.branch.startsWith("agents/")) {
        trees.push({ path: cur.path, branch: cur.branch });
      }
      cur = {};
    }
  }
  if (trees.length === 0) {
    vscode.window.showInformationMessage(
      "Agent Sessions: no hay worktrees de comparativa para comparar."
    );
    return;
  }

  const sections: string[] = [`# Comparativa de agentes\n`];
  sections.push(
    `Repo: \`${repoRoot}\` · base: \`HEAD\` · ${trees.length} agente(s)\n`
  );

  for (const t of trees) {
    sections.push(`---\n\n## ${t.branch}\n`);
    sections.push(`\`${t.path}\`\n`);
    // Working-tree diff against HEAD (uncommitted edits inside the worktree).
    try {
      const stat = (
        await exec("git", ["diff", "--stat", "HEAD"], t.path)
      ).trim();
      if (stat) {
        sections.push(`\n### Cambios sin commit\n\n\`\`\`\n${stat}\n\`\`\`\n`);
      }
    } catch {
      /* ignore: empty diff or missing repo */
    }
    // Commits the branch added on top of HEAD (from the launch base).
    try {
      const baseSha = (
        await exec("git", ["rev-parse", "HEAD"], repoRoot)
      ).trim();
      const log = (
        await exec(
          "git",
          ["log", "--oneline", `${baseSha}..HEAD`],
          t.path
        )
      ).trim();
      if (log) {
        sections.push(`\n### Commits sobre HEAD\n\n\`\`\`\n${log}\n\`\`\`\n`);
      }
    } catch {
      /* ignore */
    }
    sections.push(
      `\n[Abrir worktree](command:vscode.openFolder?${encodeURIComponent(
        JSON.stringify([vscode.Uri.file(t.path), { forceNewWindow: true }])
      )})\n`
    );
  }

  const doc = await vscode.workspace.openTextDocument({
    content: sections.join("\n"),
    language: "markdown",
  });
  await vscode.window.showTextDocument(doc, { preview: true });
}

/** List worktrees with the `agents/` branch prefix and offer to delete the
 *  ones the user selects. Soft cleanup: prunes worktrees and deletes branches,
 *  never touches committed work. */
async function cleanupWorktrees(): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    vscode.window.showWarningMessage(
      "Agent Sessions: abre primero la carpeta del repo."
    );
    return;
  }
  const repoRoot = folder.uri.fsPath;
  let raw: string;
  try {
    raw = await exec("git", ["worktree", "list", "--porcelain"], repoRoot);
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  const trees: { path: string; branch: string }[] = [];
  let cur: { path?: string; branch?: string } = {};
  for (const line of raw.split("\n")) {
    if (line.startsWith("worktree ")) cur = { path: line.slice(9) };
    else if (line.startsWith("branch ")) {
      cur.branch = line.slice(7).replace(/^refs\/heads\//, "");
    } else if (line.trim() === "" && cur.path) {
      if (cur.branch && cur.branch.startsWith("agents/")) {
        trees.push({ path: cur.path, branch: cur.branch });
      }
      cur = {};
    }
  }
  if (trees.length === 0) {
    vscode.window.showInformationMessage(
      "Agent Sessions: no hay worktrees de comparativa que limpiar."
    );
    return;
  }
  const picks = await vscode.window.showQuickPick(
    trees.map((t) => ({
      label: `$(trash) ${t.branch}`,
      description: t.path,
      tree: t,
    })),
    {
      canPickMany: true,
      placeHolder: "Worktrees a eliminar (espacio para marcar)",
    }
  );
  if (!picks || picks.length === 0) return;
  for (const p of picks) {
    try {
      await exec("git", ["worktree", "remove", "--force", p.tree.path], repoRoot);
      await exec("git", ["branch", "-D", p.tree.branch], repoRoot);
    } catch (e) {
      vscode.window.showWarningMessage(
        `Agent Sessions: no se pudo eliminar ${p.tree.branch} (${(e as Error).message}).`
      );
    }
  }
  vscode.window.showInformationMessage(
    `Agent Sessions: limpiados ${picks.length} worktree(s).`
  );
}

/** "Smart" launch: pick the provider the user most recently used in (or near)
 *  the current workspace folder. Falls back to the most-used provider overall
 *  if nothing matches the cwd. Lets users skip the proveedor picker in the
 *  90% case where they just want "the usual one for this project". */
async function smartLaunch(view: SessionsView): Promise<void> {
  let providers: ProviderInfo[];
  try {
    providers = await runCli<ProviderInfo[]>(["providers"]);
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  const usable = providers.filter((p) => p.binaryFound);
  if (usable.length === 0) {
    vscode.window.showWarningMessage(
      "Agent Sessions: ningún binario de agente encontrado en PATH."
    );
    return;
  }

  const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? null;
  const sessions: Session[] = (view as any).scan.sessions;

  // Score each usable provider: +10 for last use inside this exact cwd,
  // +1 for last use in any descendant path, with recency baked in.
  const scores = new Map<string, number>();
  for (const s of sessions) {
    if (!s.cwd) continue;
    const here =
      cwd && (s.cwd === cwd || s.cwd.startsWith(cwd + path.sep));
    const ageDays = (Date.now() / 1000 - s.lastActivity) / 86400;
    const recency = Math.max(0, 30 - ageDays); // 0..30
    const weight = here ? 10 + recency : 1 + recency * 0.1;
    scores.set(s.provider, (scores.get(s.provider) || 0) + weight);
  }
  const ranked = usable
    .map((p) => ({ p, score: scores.get(p.id) || 0 }))
    .sort((a, b) => b.score - a.score);
  const top = ranked[0];

  const action = await vscode.window.showInformationMessage(
    cwd
      ? `Agente sugerido para ${path.basename(cwd)}: ${top.p.displayName}.`
      : `Agente sugerido: ${top.p.displayName}.`,
    "Lanzar",
    "Otro…"
  );
  let chosen: ProviderInfo | null = null;
  if (action === "Lanzar") chosen = top.p;
  else if (action === "Otro…") {
    const pick = await vscode.window.showQuickPick(
      ranked.map((r) => ({
        label:
          r.p === top.p
            ? `$(star) ${r.p.displayName}`
            : `   ${r.p.displayName}`,
        description: r.score > 0 ? `score ${r.score.toFixed(1)}` : "sin uso",
        info: r.p,
      })),
      { placeHolder: "Elige otro agente" }
    );
    if (pick) chosen = pick.info;
  }
  if (!chosen) return;
  launch(`✦ ${chosen.displayName}`, cwd ?? undefined, chosen.newSessionArgv);
}

async function newSession(): Promise<void> {
  let providers: ProviderInfo[];
  try {
    providers = await runCli<ProviderInfo[]>(["providers"]);
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  const usable = providers.filter((p) => p.binaryFound);
  if (usable.length === 0) {
    vscode.window.showWarningMessage(
      "Agent Sessions: ningún binario de agente encontrado en PATH."
    );
    return;
  }
  const pick = await vscode.window.showQuickPick(
    usable.map((p) => ({ label: p.displayName, info: p })),
    { placeHolder: "Proveedor para la nueva sesión" }
  );
  if (!pick) return;
  launch(`✦ ${pick.info.displayName}`, undefined, pick.info.newSessionArgv);
}

// ── Session metadata edits ───────────────────────────────────────────────────

const COLOR_PALETTE: { label: string; hex: string | null }[] = [
  { label: "(sin color)", hex: null },
  { label: "● Rojo", hex: "#e06c75" },
  { label: "● Naranja", hex: "#d19a66" },
  { label: "● Amarillo", hex: "#e5c07b" },
  { label: "● Verde", hex: "#98c379" },
  { label: "● Turquesa", hex: "#56b6c2" },
  { label: "● Azul", hex: "#61afef" },
  { label: "● Morado", hex: "#c678dd" },
  { label: "● Rosa", hex: "#f48fb1" },
  { label: "● Gris", hex: "#9ca0a4" },
];

/** Single "More…" menu for a session. Replaces the inline TreeItem actions
 *  we used to expose via contextValue + view/item/context. */
async function sessionContextMenu(view: SessionsView, s: Session): Promise<void> {
  const meta = view.metadataFor(s.provider, s.id);
  const items: { label: string; action: string }[] = [
    { label: "$(play) Reanudar", action: "resume" },
    { label: "$(comment) Reanudar con prompt…", action: "resumeWithPrompt" },
    { label: "$(arrow-swap) Continuar en otro agente…", action: "switch" },
    { label: "$(eye) Previsualizar", action: "preview" },
    {
      label: meta?.favorite ? "$(star-full) Quitar favorito" : "$(star) Marcar favorito",
      action: "favorite",
    },
    { label: "$(edit) Renombrar…", action: "rename" },
    { label: "$(note) Notas…", action: "notes" },
    { label: "$(tag) Etiquetas…", action: "tags" },
    { label: "$(symbol-color) Color…", action: "color" },
    { label: "$(cloud-download) Exportar a .zip…", action: "export" },
  ];
  if (meta) items.push({ label: "Limpiar metadata", action: "clear" });
  items.push({ label: "$(trash) Eliminar sesión…", action: "delete" });
  const pick = await vscode.window.showQuickPick(items, {
    placeHolder: s.title || s.id,
  });
  if (!pick) return;
  switch (pick.action) {
    case "resume":
      return resumeSession(view, s, meta);
    case "preview":
      return preview(s);
    case "rename":
      return renameSession(view, s);
    case "tags":
      return editTags(view, s);
    case "color":
      return setSessionColor(view, s);
    case "export":
      return exportSession(s);
    case "notes":
      return editNotes(view, s);
    case "favorite":
      return toggleFavorite(view, s);
    case "clear":
      return clearMetadata(view, s);
    case "delete":
      return deleteSession(view, s);
    case "resumeWithPrompt":
      return resumeWithPrompt(view, s, meta);
    case "switch":
      return continueAsOtherAgent(view, s);
  }
}

/** Resume the session and immediately push a new prompt into the terminal —
 *  saves the user from re-typing context for follow-up questions. */
async function resumeWithPrompt(
  view: SessionsView,
  s: Session,
  meta: SessionMetadata | null
): Promise<void> {
  const prompt = await vscode.window.showInputBox({
    title: "Reanudar con prompt",
    prompt: "El agente se reanuda y se envía este texto como siguiente turno.",
  });
  if (!prompt || !prompt.trim()) return;
  // Reuse the existing terminal if any (avoids the double-resume corruption),
  // otherwise fall back to launching a fresh one.
  const reused = view.focusActiveTerminal(s.provider, s.id);
  const terminal = reused
    ? findTerminalFor(view, s.provider, s.id)!
    : (() => {
        const name = (meta?.name?.trim() || s.title?.trim() || s.provider).slice(
          0,
          30
        );
        const t = launch(`▶ ${name}`, s.cwd, s.resumeArgv);
        if (t) view.registerTerminal(s.provider, s.id, t);
        return t;
      })();
  if (!terminal) return;
  // Same 2.5 s grace as launchParallel — TUIs need a moment to render their
  // input area. We send WITHOUT the trailing newline so the user can review
  // before submitting (most agent TUIs require Enter explicitly).
  setTimeout(() => terminal.sendText(prompt, false), reused ? 100 : 2500);
}

function findTerminalFor(
  view: SessionsView,
  provider: string,
  id: string
): vscode.Terminal | undefined {
  return (view as any).activeTerminals.get(`${provider}:${id}`);
}

/** Cross-provider continuation: take the last user/assistant exchange of a
 *  Claude session and seed a fresh Codex/OpenCode/Gemini session with it.
 *  Doesn't reuse the original transcript (no canonical IR yet) — instead the
 *  new agent gets a "here's where we were" handoff prompt. */
async function continueAsOtherAgent(view: SessionsView, s: Session): Promise<void> {
  let providers: ProviderInfo[];
  try {
    providers = await runCli<ProviderInfo[]>(["providers"]);
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  const targets = providers.filter(
    (p) => p.binaryFound && p.id !== s.provider
  );
  if (targets.length === 0) {
    vscode.window.showWarningMessage(
      "Agent Sessions: no hay otros agentes disponibles en PATH."
    );
    return;
  }
  const pick = await vscode.window.showQuickPick(
    targets.map((p) => ({ label: p.displayName, info: p })),
    { placeHolder: "Continuar en…" }
  );
  if (!pick) return;

  let turns: PreviewTurn[] = [];
  try {
    turns = (await runCli<PreviewTurn[]>(["preview", s.provider, s.id])) || [];
  } catch (e) {
    vscode.window.showWarningMessage(
      `Agent Sessions: no se pudo leer el historial (${(e as Error).message}).`
    );
    return;
  }
  if (turns.length === 0) {
    vscode.window.showWarningMessage(
      "Agent Sessions: la sesión no tiene contenido legible para migrar."
    );
    return;
  }
  // Keep the handoff prompt short: the last ~3 turns are plenty of context
  // for the target agent to pick up the thread.
  const tail = turns.slice(-3);
  const handoff = buildHandoff(s, tail, pick.info.displayName);

  const terminal = launch(
    `🔀 ${pick.info.displayName}`,
    s.cwd,
    pick.info.newSessionArgv
  );
  if (!terminal) return;
  setTimeout(() => terminal.sendText(handoff, false), 2500);
  vscode.window.showInformationMessage(
    `Agent Sessions: handoff de ${s.provider} → ${pick.info.id} preparado en el terminal (pulsa Enter para enviarlo).`
  );
}

function buildHandoff(s: Session, turns: PreviewTurn[], targetName: string): string {
  const header = `Continúo una conversación previa que mantuve con ${s.provider}` +
    (s.title ? ` sobre: ${s.title}` : "") +
    `. Aquí tienes el contexto reciente (${turns.length} turno${
      turns.length === 1 ? "" : "s"
    } finales). Por favor, sigue desde donde se quedó.`;
  const body = turns
    .map((t) => {
      const role = t.role === "user" ? "USUARIO" : "ASISTENTE";
      // Truncate per turn so the handoff doesn't blow past the new agent's
      // input buffer. A few KB total is enough for a clean pickup.
      const text = (t.text || "").trim().slice(0, 2000);
      return `--- ${role} ---\n${text}`;
    })
    .join("\n\n");
  return `${header}\n\n${body}\n\n--- TURNO ACTUAL (${targetName}) ---\nContinúa.`;
}

async function editNotes(view: SessionsView, s: Session): Promise<void> {
  const meta = view.metadataFor(s.provider, s.id);
  const value = await vscode.window.showInputBox({
    title: "Notas de la sesión",
    prompt: "Texto libre (vacío para limpiar)",
    value: meta?.notes ?? "",
  });
  if (value === undefined) return;
  await patchMetadata(view, s.provider, s.id, {
    notes: value.trim() === "" ? null : value,
  });
}

async function toggleFavorite(view: SessionsView, s: Session): Promise<void> {
  const meta = view.metadataFor(s.provider, s.id);
  await patchMetadata(view, s.provider, s.id, { favorite: !meta?.favorite });
}

/** Two-step destructive flow: a warning modal, then a second prompt if the
 *  sidecar reports `ACTIVE` (provider's live registry says the session is
 *  running). The native panel does the same dance. */
async function deleteSession(view: SessionsView, s: Session): Promise<void> {
  const label = (view.metadataFor(s.provider, s.id)?.name || s.title || s.id).slice(0, 60);
  const confirm = await vscode.window.showWarningMessage(
    `¿Eliminar "${label}"? Esta acción no se puede deshacer.`,
    { modal: true },
    "Eliminar"
  );
  if (confirm !== "Eliminar") return;
  try {
    await runCli(["delete", s.provider, s.id]);
    afterDelete(view, s);
  } catch (e) {
    const msg = (e as Error).message;
    if (msg.trim() === "ACTIVE") {
      const force = await vscode.window.showWarningMessage(
        `La sesión está activa. ¿Forzar el borrado?`,
        { modal: true },
        "Forzar borrado"
      );
      if (force !== "Forzar borrado") return;
      try {
        await runCli(["delete", s.provider, s.id, "--force"]);
        afterDelete(view, s);
      } catch (e2) {
        vscode.window.showErrorMessage(
          `Agent Sessions: no se pudo borrar (${(e2 as Error).message}).`
        );
      }
    } else {
      vscode.window.showErrorMessage(`Agent Sessions: ${msg}.`);
    }
  }
}

/** Local cleanup after a successful delete: drop the session from the cached
 *  scan plus any metadata, then push so the card disappears immediately. */
function afterDelete(view: SessionsView, s: Session): void {
  const idx = (view as any).scan.sessions.findIndex(
    (x: Session) => x.provider === s.provider && x.id === s.id
  );
  if (idx >= 0) (view as any).scan.sessions.splice(idx, 1);
  delete (view as any).metadata[`${s.provider}:${s.id}`];
  view.push();
}

async function renameSession(view: SessionsView, s: Session): Promise<void> {
  const meta = view.metadataFor(s.provider, s.id);
  const value = await vscode.window.showInputBox({
    title: "Renombrar sesión",
    prompt: "Nombre local (vacío para limpiar)",
    value: meta?.name ?? "",
  });
  if (value === undefined) return;
  await patchMetadata(view, s.provider, s.id, {
    name: value.trim() === "" ? null : value.trim(),
  });
}

async function editTags(view: SessionsView, s: Session): Promise<void> {
  const meta = view.metadataFor(s.provider, s.id);
  const value = await vscode.window.showInputBox({
    title: "Etiquetas",
    prompt: "Separadas por coma o espacio (vacío para limpiar)",
    value: (meta?.tags ?? []).join(", "),
  });
  if (value === undefined) return;
  const tags = value
    .split(/[ ,]+/)
    .map((t) => t.trim())
    .filter((t, i, a) => t.length > 0 && a.indexOf(t) === i);
  await patchMetadata(view, s.provider, s.id, { tags });
}

async function setSessionColor(view: SessionsView, s: Session): Promise<void> {
  const pick = await vscode.window.showQuickPick(
    COLOR_PALETTE.map((c) => ({ label: c.label, hex: c.hex })),
    { placeHolder: "Color para la sesión" }
  );
  if (!pick) return;
  await patchMetadata(view, s.provider, s.id, {
    color: pick.hex ?? null,
  });
}

async function clearMetadata(view: SessionsView, s: Session): Promise<void> {
  try {
    await runCli(["metadata-clear", s.provider, s.id]);
    view.applyMetadata(s.provider, s.id, null);
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: no se pudo limpiar metadata (${(e as Error).message}).`
    );
  }
}

/** A `SessionMetadata`-shaped patch — same fields, only the ones present are
 *  applied, `null` or `false` clear, omission leaves untouched. */
interface MetadataPatch {
  name?: string | null;
  tags?: string[];
  color?: string | null;
  notes?: string | null;
  favorite?: boolean;
}

async function patchMetadata(
  view: SessionsView,
  provider: string,
  id: string,
  patch: MetadataPatch
): Promise<void> {
  try {
    const next = await runCli<SessionMetadata | null>(
      ["metadata-set", provider, id],
      JSON.stringify(patch)
    );
    view.applyMetadata(provider, id, next);
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: no se pudo guardar metadata (${(e as Error).message}).`
    );
  }
}

// ── Project metadata edits ───────────────────────────────────────────────────

async function pickProjectCwd(view: SessionsView, cwd?: string): Promise<string | undefined> {
  if (cwd && cwd !== "(sin proyecto)") return cwd;
  const seen = new Set<string>();
  for (const s of view.sessionsSnapshot()) {
    if (s.cwd) seen.add(s.cwd);
  }
  const items = Array.from(seen)
    .sort()
    .map((c) => ({
      label: path.basename(c) || c,
      description: displayPath(c),
      cwd: c,
    }));
  const pick = await vscode.window.showQuickPick(items, {
    placeHolder: "Proyecto a editar",
  });
  return pick?.cwd;
}

async function renameProject(
  view: SessionsView,
  arg: { cwd?: string } = {}
): Promise<void> {
  const cwd = await pickProjectCwd(view, arg.cwd);
  if (!cwd) return;
  const current = view.projectAliasFor(cwd) ?? "";
  const value = await vscode.window.showInputBox({
    title: "Renombrar proyecto",
    prompt: `${displayPath(cwd)} — alias local (vacío para limpiar)`,
    value: current,
  });
  if (value === undefined) return;
  await patchProject(view, cwd, {
    name: value.trim() === "" ? null : value.trim(),
  });
}

async function setProjectColor(
  view: SessionsView,
  arg: { cwd?: string } = {}
): Promise<void> {
  const cwd = await pickProjectCwd(view, arg.cwd);
  if (!cwd) return;
  const pick = await vscode.window.showQuickPick(
    COLOR_PALETTE.map((c) => ({ label: c.label, hex: c.hex })),
    { placeHolder: `${displayPath(cwd)} — color del proyecto` }
  );
  if (!pick) return;
  await patchProject(view, cwd, { color: pick.hex });
}

async function clearProjectMetadata(
  view: SessionsView,
  arg: { cwd?: string } = {}
): Promise<void> {
  const cwd = await pickProjectCwd(view, arg.cwd);
  if (!cwd) return;
  try {
    await runCli(["projects-clear", cwd]);
    view.applyProject(cwd, null);
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: no se pudo limpiar el proyecto (${(e as Error).message}).`
    );
  }
}

async function patchProject(
  view: SessionsView,
  cwd: string,
  patch: ProjectPatch
): Promise<void> {
  try {
    const next = await runCli<{ name: string | null; color: string | null } | null>(
      ["projects-set", cwd],
      JSON.stringify(patch)
    );
    view.applyProject(cwd, next);
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: no se pudo guardar el proyecto (${(e as Error).message}).`
    );
  }
}

function displayPath(p: string): string {
  const home = os.homedir();
  if (p === home) return "~";
  if (p.startsWith(home + path.sep)) return "~" + p.slice(home.length);
  return p;
}

// ── Move (drag & drop between projects, Claude-only) ────────────────────────

/** Relocate a Claude session's jsonl from one project directory into another.
 *  The sidecar's `move` command does the file move and enforces the live-
 *  session / collision guards; we just translate the errors into messages. */
async function moveSessionToProject(
  view: SessionsView,
  id: string,
  sourceCwd: string,
  destCwd: string
): Promise<void> {
  if (!id || !sourceCwd || !destCwd || sourceCwd === destCwd) return;
  try {
    await runCli(["move", id, sourceCwd, destCwd]);
    vscode.window.showInformationMessage(
      `Agent Sessions: movida a ${path.basename(destCwd) || destCwd}.`
    );
    await view.refresh();
  } catch (e) {
    const raw = (e as Error).message.trim();
    let msg: string;
    if (raw === "ACTIVE")
      msg = "La sesión está activa: ciérrala primero para moverla.";
    else if (raw === "COLLISION")
      msg = "Ya existe una sesión con ese id en el proyecto destino.";
    else msg = `No se pudo mover: ${raw}`;
    vscode.window.showWarningMessage(`Agent Sessions: ${msg}`);
  }
}

// ── Export / import ──────────────────────────────────────────────────────────

async function exportSession(s: Session): Promise<void> {
  const defaultName = `aterm-export-${s.provider}-${s.id.slice(0, 8)}.zip`;
  const target = await vscode.window.showSaveDialog({
    title: "Exportar sesión",
    defaultUri: vscode.Uri.file(path.join(os.homedir(), defaultName)),
    filters: { "Session archive": ["zip"] },
  });
  if (!target) return;
  try {
    const result = await runCli<{ written: number; dest: string }>([
      "export",
      s.provider,
      s.id,
      target.fsPath,
    ]);
    if (result.written === 0) {
      vscode.window.showWarningMessage(
        "Agent Sessions: nada que exportar (sesión no localizada en disco)."
      );
    } else {
      vscode.window.showInformationMessage(
        `Agent Sessions: exportada a ${result.dest}`
      );
    }
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: export falló (${(e as Error).message}).`
    );
  }
}

async function importArchive(view: SessionsView): Promise<void> {
  const picked = await vscode.window.showOpenDialog({
    title: "Importar archivo de sesiones (.zip) — solo Claude",
    canSelectFiles: true,
    canSelectFolders: false,
    canSelectMany: false,
    filters: { "Session archive": ["zip"] },
  });
  if (!picked || picked.length === 0) return;
  try {
    const outcome = await runCli<ImportOutcome>(["import", picked[0].fsPath]);
    const parts = [
      `Importadas ${outcome.imported.length}`,
      `omitidas ${outcome.skippedExisting.length} existentes`,
      `${outcome.skippedMissing.length} sin datos`,
    ];
    vscode.window.showInformationMessage(`Agent Sessions: ${parts.join(", ")}.`);
    await view.refresh();
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: import falló (${(e as Error).message}).`
    );
  }
}

// ── Full-text search in conversation content ────────────────────────────────

interface ContentHit {
  provider: string;
  id: string;
  title: string | null;
  cwd: string | null;
  snippet: string;
  lastActivity: number;
}

/** Search inside conversation transcripts (heavier than the in-memory
 *  metadata filter). Shows hits as a QuickPick; picking one opens the
 *  preview document for that session. */
async function searchContent(view: SessionsView): Promise<void> {
  const query = await vscode.window.showInputBox({
    title: "Buscar en el contenido de las conversaciones",
    prompt: "Substring case-insensitive. Recorre los transcripts de cada sesión.",
  });
  if (!query || !query.trim()) return;

  const status = vscode.window.setStatusBarMessage(
    "Agent Sessions: buscando…"
  );
  let hits: ContentHit[] = [];
  try {
    hits = (await runCli<ContentHit[]>(["search-content", query])) || [];
  } catch (e) {
    status.dispose();
    vscode.window.showErrorMessage(
      `Agent Sessions: búsqueda falló (${(e as Error).message}).`
    );
    return;
  }
  status.dispose();
  if (hits.length === 0) {
    vscode.window.showInformationMessage(
      `Agent Sessions: sin resultados para "${query}".`
    );
    return;
  }
  const pick = await vscode.window.showQuickPick(
    hits.map((h) => ({
      label: `$(comment-discussion) ${h.title || h.id.slice(0, 8)}`,
      description: h.provider,
      detail: h.snippet,
      hit: h,
    })),
    { placeHolder: `${hits.length} resultado(s) — selecciona para previsualizar` }
  );
  if (!pick) return;
  const session = (view as any).scan.sessions.find(
    (s: Session) => s.provider === pick.hit.provider && s.id === pick.hit.id
  ) as Session | undefined;
  if (session) await preview(session);
}

// ── Launch templates ────────────────────────────────────────────────────────

interface LaunchTemplate {
  id: string;
  name: string;
  provider: string;
  prompt?: string;
  cwd?: string;
  tags?: string[];
}

/** Save the current focus as a template: user types a name, picks a provider,
 *  optionally a prompt and cwd. Then it shows up in `runTemplate` for
 *  one-click relaunch. */
async function saveTemplate(): Promise<void> {
  let providers: ProviderInfo[];
  try {
    providers = await runCli<ProviderInfo[]>(["providers"]);
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  const usable = providers.filter((p) => p.binaryFound);
  if (usable.length === 0) {
    vscode.window.showWarningMessage(
      "Agent Sessions: ningún agente disponible para asociar a la plantilla."
    );
    return;
  }
  const name = await vscode.window.showInputBox({
    title: "Nombre de la plantilla",
    placeHolder: "p. ej. Revisión rápida con Claude",
  });
  if (!name || !name.trim()) return;
  const pick = await vscode.window.showQuickPick(
    usable.map((p) => ({ label: p.displayName, info: p })),
    { placeHolder: "Proveedor" }
  );
  if (!pick) return;
  const prompt = await vscode.window.showInputBox({
    title: "Prompt inicial (opcional)",
    placeHolder: "Vacío = sólo lanza el agente",
  });
  if (prompt === undefined) return;
  const cwd =
    vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? undefined;
  const id = `tpl-${Date.now().toString(36)}`;
  const tpl: LaunchTemplate = {
    id,
    name: name.trim(),
    provider: pick.info.id,
    prompt: prompt.trim() === "" ? undefined : prompt,
    cwd,
  };
  try {
    await runCli(["templates-set", id], JSON.stringify(tpl));
    vscode.window.showInformationMessage(
      `Agent Sessions: plantilla "${tpl.name}" guardada.`
    );
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: no se pudo guardar (${(e as Error).message}).`
    );
  }
}

async function runTemplate(): Promise<void> {
  let templates: LaunchTemplate[];
  try {
    templates = (await runCli<LaunchTemplate[]>(["templates-get"])) || [];
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  if (templates.length === 0) {
    vscode.window.showInformationMessage(
      "Agent Sessions: aún no tienes plantillas. Usa “Guardar plantilla…”."
    );
    return;
  }
  const pick = await vscode.window.showQuickPick(
    templates.map((t) => ({
      label: `$(rocket) ${t.name}`,
      description: t.provider,
      detail: t.prompt
        ? t.prompt.length > 80
          ? t.prompt.slice(0, 80) + "…"
          : t.prompt
        : "(sin prompt)",
      tpl: t,
    })),
    { placeHolder: "Plantilla a lanzar" }
  );
  if (!pick) return;
  const t = pick.tpl;
  // Resolve the argv for the chosen provider.
  let providers: ProviderInfo[];
  try {
    providers = await runCli<ProviderInfo[]>(["providers"]);
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  const info = providers.find((p) => p.id === t.provider);
  if (!info || !info.binaryFound) {
    vscode.window.showWarningMessage(
      `Agent Sessions: el binario de ${t.provider} no está disponible.`
    );
    return;
  }
  const terminal = launch(
    `🚀 ${t.name.slice(0, 30)}`,
    t.cwd,
    info.newSessionArgv
  );
  if (terminal && t.prompt) {
    setTimeout(() => terminal.sendText(t.prompt!, false), 2500);
  }
}

async function manageTemplates(): Promise<void> {
  let templates: LaunchTemplate[];
  try {
    templates = (await runCli<LaunchTemplate[]>(["templates-get"])) || [];
  } catch (e) {
    vscode.window.showErrorMessage(`Agent Sessions: ${(e as Error).message}`);
    return;
  }
  if (templates.length === 0) {
    vscode.window.showInformationMessage(
      "Agent Sessions: no hay plantillas que gestionar."
    );
    return;
  }
  const picks = await vscode.window.showQuickPick(
    templates.map((t) => ({
      label: `$(trash) ${t.name}`,
      description: t.provider,
      tpl: t,
    })),
    {
      canPickMany: true,
      placeHolder: "Plantillas a eliminar (espacio para marcar)",
    }
  );
  if (!picks || picks.length === 0) return;
  for (const p of picks) {
    try {
      await runCli(["templates-delete", p.tpl.id]);
    } catch (e) {
      vscode.window.showWarningMessage(
        `Agent Sessions: no se pudo borrar ${p.tpl.name} (${(e as Error).message}).`
      );
    }
  }
  vscode.window.showInformationMessage(
    `Agent Sessions: borradas ${picks.length} plantilla(s).`
  );
}

// ── Catalog backup / restore (cross-machine) ────────────────────────────────

async function backupCatalog(): Promise<void> {
  const target = await vscode.window.showSaveDialog({
    title: "Backup del catálogo de Agent Sessions",
    defaultUri: vscode.Uri.file(
      path.join(os.homedir(), `aterm-catalog-${Date.now().toString(36)}.zip`)
    ),
    filters: { "Catalog backup": ["zip"] },
  });
  if (!target) return;
  try {
    const result = await runCli<{ written: number; dest: string }>([
      "backup",
      target.fsPath,
    ]);
    vscode.window.showInformationMessage(
      `Agent Sessions: backup escrito (${result.written} ficheros) → ${result.dest}`
    );
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: backup falló (${(e as Error).message}).`
    );
  }
}

async function restoreCatalog(view: SessionsView): Promise<void> {
  const picked = await vscode.window.showOpenDialog({
    title: "Restaurar catálogo (.zip) — sobrescribe metadata/proyectos/templates",
    canSelectFiles: true,
    canSelectMany: false,
    filters: { "Catalog backup": ["zip"] },
  });
  if (!picked || picked.length === 0) return;
  const confirm = await vscode.window.showWarningMessage(
    "Esto sobrescribe tu metadata local (rename/tags/color/notas/favoritos), alias de proyectos y plantillas. ¿Continuar?",
    { modal: true },
    "Restaurar"
  );
  if (confirm !== "Restaurar") return;
  try {
    const outcome = await runCli<{ restored: string[] }>([
      "restore",
      picked[0].fsPath,
    ]);
    vscode.window.showInformationMessage(
      `Agent Sessions: restaurados ${outcome.restored.join(", ") || "(ningún fichero)"}.`
    );
    await view.refresh();
  } catch (e) {
    vscode.window.showErrorMessage(
      `Agent Sessions: restore falló (${(e as Error).message}).`
    );
  }
}

// ── Filter / group commands (palette) ────────────────────────────────────────

async function setFilter(view: SessionsView): Promise<void> {
  const value = await vscode.window.showInputBox({
    title: "Filtrar sesiones",
    prompt: "Coincide con título, nombre, cwd, rama o etiquetas",
    value: view.currentFilter(),
  });
  if (value === undefined) return;
  // Push the value back to the webview, which owns the UI state.
  (view as any).filter = value;
  view.push();
}

async function setGroupBy(view: SessionsView): Promise<void> {
  const current = currentGroupMode();
  const options: { label: string; value: GroupMode }[] = [
    { label: "Proveedor", value: "provider" },
    { label: "Proyecto", value: "project" },
    { label: "Cascada", value: "cascade" },
  ];
  const pick = await vscode.window.showQuickPick(
    options.map((o) => ({
      ...o,
      label: o.value === current ? `● ${o.label}` : `  ${o.label}`,
    })),
    { placeHolder: "Agrupar el árbol por…" }
  );
  if (!pick) return;
  await vscode.workspace
    .getConfiguration("agentSessions")
    .update("groupBy", pick.value, vscode.ConfigurationTarget.Global);
  view.push();
}

// ── Activation ───────────────────────────────────────────────────────────────

export function activate(context: vscode.ExtensionContext): void {
  extensionPath = context.extensionPath;
  output = vscode.window.createOutputChannel("Agent Sessions");
  context.subscriptions.push(output);
  log("Agent Sessions activated.");
  const view = new SessionsView(context);
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(SessionsView.viewType, view, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    vscode.commands.registerCommand("agentSessions.refresh", () => view.refresh()),
    vscode.commands.registerCommand("agentSessions.focus", () =>
      // VS Code generates `<viewId>.focus` automatically for every contributed
      // view; we wrap it so the status-bar button has a single stable name.
      vscode.commands.executeCommand("agentSessions.sessions.focus")
    ),
    vscode.commands.registerCommand("agentSessions.newSession", newSession),
    vscode.commands.registerCommand("agentSessions.smartLaunch", () =>
      smartLaunch(view)
    ),
    vscode.commands.registerCommand("agentSessions.import", () => importArchive(view)),
    vscode.commands.registerCommand("agentSessions.launchParallel", launchParallel),
    vscode.commands.registerCommand("agentSessions.compareWorktrees", compareWorktrees),
    vscode.commands.registerCommand("agentSessions.cleanupWorktrees", cleanupWorktrees),
    vscode.commands.registerCommand("agentSessions.backupCatalog", backupCatalog),
    vscode.commands.registerCommand("agentSessions.restoreCatalog", () =>
      restoreCatalog(view)
    ),
    vscode.commands.registerCommand("agentSessions.searchContent", () =>
      searchContent(view)
    ),
    vscode.commands.registerCommand("agentSessions.runTemplate", runTemplate),
    vscode.commands.registerCommand("agentSessions.saveTemplate", saveTemplate),
    vscode.commands.registerCommand("agentSessions.manageTemplates", manageTemplates),
    vscode.commands.registerCommand("agentSessions.setFilter", () => setFilter(view)),
    vscode.commands.registerCommand("agentSessions.clearFilter", () => {
      (view as any).filter = "";
      view.push();
    }),
    vscode.commands.registerCommand("agentSessions.setGroupBy", () => setGroupBy(view)),
    vscode.commands.registerCommand("agentSessions.renameProject", (cwd?: string) =>
      renameProject(view, { cwd })
    ),
    vscode.commands.registerCommand("agentSessions.setProjectColor", (cwd?: string) =>
      setProjectColor(view, { cwd })
    ),
    vscode.commands.registerCommand(
      "agentSessions.clearProjectMetadata",
      (cwd?: string) => clearProjectMetadata(view, { cwd })
    ),
    // Re-render if the user toggles groupBy/costAlertDaily via settings.json.
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (
        e.affectsConfiguration("agentSessions.groupBy") ||
        e.affectsConfiguration("agentSessions.costAlertDaily")
      )
        view.push();
    })
  );
}

export function deactivate(): void {}
