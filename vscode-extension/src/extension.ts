// Agent Sessions — VS Code extension.
//
// A thin UI over the `agent-sessions-cli` sidecar (shared Rust core with the
// native `aterm` app). The heavy lifting — discovering / parsing / locating
// coding-agent sessions on disk — lives in the sidecar; this extension only
// renders a tree and drives the integrated terminal. That's the whole point of
// the VS Code port: the editor already provides the terminal, so we reuse the
// session-manager half and skip the emulator entirely.

import * as cp from "child_process";
import * as path from "path";
import * as vscode from "vscode";

/** One session as emitted by `agent-sessions-cli scan` (camelCase JSON). */
interface Session {
  provider: string;
  id: string;
  title: string | null;
  cwd: string | null;
  branch: string | null;
  messageCount: number | null;
  lastActivity: number;
  isActive: boolean;
  model: string | null;
  resumeArgv: string[];
}

interface ProviderInfo {
  id: string;
  displayName: string;
  available: boolean;
  binaryFound: boolean;
  newSessionArgv: string[];
}

interface ScanResult {
  providers: ProviderInfo[];
  sessions: Session[];
}

interface PreviewTurn {
  role: string;
  text: string;
}

// ── Sidecar invocation ─────────────────────────────────────────────────────

function cliPath(): string {
  return vscode.workspace
    .getConfiguration("agentSessions")
    .get<string>("cliPath", "agent-sessions-cli");
}

/** Run the sidecar and parse its JSON stdout. Rejects on non-zero exit. */
function runCli<T>(args: string[]): Promise<T> {
  return new Promise((resolve, reject) => {
    cp.execFile(
      cliPath(),
      args,
      { maxBuffer: 32 * 1024 * 1024 },
      (err, stdout, stderr) => {
        if (err) {
          reject(new Error(stderr.trim() || err.message));
          return;
        }
        try {
          resolve(JSON.parse(stdout) as T);
        } catch (e) {
          reject(new Error(`salida no-JSON del sidecar: ${e}`));
        }
      }
    );
  });
}

// ── Tree model ─────────────────────────────────────────────────────────────

type Node = ProviderNode | SessionNode;

class ProviderNode {
  readonly kind = "provider";
  constructor(
    readonly info: ProviderInfo,
    readonly sessions: Session[]
  ) {}
}

class SessionNode {
  readonly kind = "session";
  constructor(readonly session: Session) {}
}

class SessionsProvider implements vscode.TreeDataProvider<Node> {
  private readonly _onDidChange = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChange.event;

  private scan: ScanResult = { providers: [], sessions: [] };
  private error: string | null = null;

  async refresh(): Promise<void> {
    try {
      this.scan = await runCli<ScanResult>(["scan"]);
      this.error = null;
    } catch (e) {
      this.error = (e as Error).message;
      this.scan = { providers: [], sessions: [] };
      vscode.window.showErrorMessage(`Agent Sessions: ${this.error}`);
    }
    this._onDidChange.fire();
  }

  getTreeItem(node: Node): vscode.TreeItem {
    return node.kind === "provider"
      ? providerItem(node)
      : sessionItem(node.session);
  }

  getChildren(node?: Node): Node[] {
    if (!node) {
      // Top level: one group per provider that has sessions, in scan order.
      return this.scan.providers
        .map((info) => {
          const sessions = this.scan.sessions.filter(
            (s) => s.provider === info.id
          );
          return new ProviderNode(info, sessions);
        })
        .filter((p) => p.sessions.length > 0);
    }
    if (node.kind === "provider") {
      return node.sessions.map((s) => new SessionNode(s));
    }
    return [];
  }
}

// ── Tree item rendering ──────────────────────────────────────────────────────

function providerItem(node: ProviderNode): vscode.TreeItem {
  const item = new vscode.TreeItem(
    node.info.displayName,
    vscode.TreeItemCollapsibleState.Expanded
  );
  item.description = `${node.sessions.length}`;
  item.contextValue = "provider";
  item.iconPath = new vscode.ThemeIcon("server-process");
  return item;
}

function sessionItem(s: Session): vscode.TreeItem {
  const label = s.title?.trim() || `(sin título) ${s.id.slice(0, 8)}`;
  const item = new vscode.TreeItem(label, vscode.TreeItemCollapsibleState.None);
  const bits = [relativeTime(s.lastActivity)];
  if (s.cwd) bits.push(path.basename(s.cwd));
  if (s.branch) bits.push(s.branch);
  item.description = bits.join(" · ");
  item.tooltip = new vscode.MarkdownString(
    [
      `**${label}**`,
      "",
      `- Proveedor: \`${s.provider}\``,
      s.model ? `- Modelo: \`${s.model}\`` : "",
      s.cwd ? `- Directorio: \`${s.cwd}\`` : "",
      s.messageCount != null ? `- Mensajes: ${s.messageCount}` : "",
      `- Última actividad: ${new Date(s.lastActivity * 1000).toLocaleString()}`,
    ]
      .filter(Boolean)
      .join("\n")
  );
  item.contextValue = "session";
  item.iconPath = new vscode.ThemeIcon(s.isActive ? "circle-filled" : "comment-discussion");
  // Default click resumes — the common action.
  item.command = {
    command: "agentSessions.resume",
    title: "Reanudar en terminal",
    arguments: [item],
  };
  (item as any).session = s;
  return item;
}

function relativeTime(unixSeconds: number): string {
  const secs = Math.max(0, Date.now() / 1000 - unixSeconds);
  const mins = Math.floor(secs / 60);
  if (mins < 1) return "ahora";
  if (mins < 60) return `hace ${mins} min`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `hace ${hours} h`;
  const days = Math.floor(hours / 24);
  return `hace ${days} d`;
}

// ── Commands ─────────────────────────────────────────────────────────────────

/** A SessionNode arrives directly from the inline menu, or a TreeItem carrying
 *  `.session` from the default-click command. Normalise to the Session. */
function sessionFrom(arg: unknown): Session | undefined {
  if (arg instanceof SessionNode) return arg.session;
  const maybe = (arg as { session?: Session })?.session;
  return maybe;
}

/** POSIX-quote an argv so the integrated terminal runs it intact. */
function shellJoin(argv: string[]): string {
  return argv
    .map((a) => (/^[\w./:=-]+$/.test(a) ? a : `'${a.replace(/'/g, `'\\''`)}'`))
    .join(" ");
}

function launch(name: string, cwd: string | null | undefined, argv: string[]): void {
  if (argv.length === 0) {
    vscode.window.showWarningMessage(
      "Agent Sessions: no hay comando para esta acción (¿binario del proveedor en PATH?)."
    );
    return;
  }
  const terminal = vscode.window.createTerminal({
    name,
    cwd: cwd ?? undefined,
  });
  terminal.show();
  terminal.sendText(shellJoin(argv), true);
}

async function resume(arg: unknown): Promise<void> {
  const s = sessionFrom(arg);
  if (!s) return;
  const name = s.title?.trim() ? s.title.slice(0, 30) : s.provider;
  launch(`▶ ${name}`, s.cwd, s.resumeArgv);
}

async function preview(arg: unknown): Promise<void> {
  const s = sessionFrom(arg);
  if (!s) return;
  try {
    const turns = await runCli<PreviewTurn[]>(["preview", s.provider, s.id]);
    const md = turns
      .map((t) => `### ${t.role === "user" ? "🧑 Usuario" : "🤖 Asistente"}\n\n${t.text}`)
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

// ── Activation ───────────────────────────────────────────────────────────────

export function activate(context: vscode.ExtensionContext): void {
  const provider = new SessionsProvider();
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("agentSessions.sessions", provider),
    vscode.commands.registerCommand("agentSessions.refresh", () => provider.refresh()),
    vscode.commands.registerCommand("agentSessions.resume", resume),
    vscode.commands.registerCommand("agentSessions.preview", preview),
    vscode.commands.registerCommand("agentSessions.newSession", newSession)
  );
  void provider.refresh();
}

export function deactivate(): void {}
