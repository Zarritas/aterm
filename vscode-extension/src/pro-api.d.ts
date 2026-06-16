// Contract between the open-source Community core and the private Pro module.
//
// The public extension implements `ProApi` (the surface Pro features need) and,
// at activation, tries to load an optional compiled `./pro` module that
// implements `ProModule`. In the Community build that module is absent, so the
// Pro commands fall back to an "edición Pro" notice. In the official build
// (produced from the private `aterm-pro` repo) the module is present and the
// real features run.
//
// Keep this contract small and stable: it's the only coupling between the two
// repos.

import type * as vscode from "vscode";

/** Minimal session shape the Pro module reads (a subset of the core's Session). */
export interface SessionLite {
  provider: string;
  id: string;
  title: string | null;
  cwd: string | null;
  model?: string | null;
  costUsd?: number | null;
  lastActivity?: number;
  contextTokens?: number | null;
  messageCount?: number | null;
}

/** One conversation turn (from the sidecar preview). */
export interface PreviewTurnLite {
  role: string;
  text: string;
}

/** Helpers the Pro module borrows from the core so it doesn't reimplement them. */
export interface ProApi {
  /** Invoke the sidecar and parse its JSON stdout. */
  runCli<T>(args: string[], stdin?: string): Promise<T>;
  /** Run an external command (e.g. git), resolving stdout / rejecting stderr. */
  exec(file: string, args: string[], cwd?: string): Promise<string>;
  /** Quote an argv into a shell line. */
  shellJoin(argv: string[]): string;
  /** Launch an agent argv in a terminal (honours openInEditor/closeOnExit). */
  launch(
    name: string,
    cwd: string | null | undefined,
    argv: string[]
  ): vscode.Terminal | null;
  /** Notifications, gated by the user's notificationLevel. */
  notifyInfo(message: string): void;
  notifyWarn(message: string): void;
  notifyError(message: string): void;
  /** Pick where to launch (workspace / known cwd / browse). undefined=cancel,
   *  null=no cwd, string=path. */
  pickLaunchCwd(providerId: string): Promise<string | null | undefined>;
  /** Split a free-text tag input into a clean tag list. */
  parseTagInput(value: string): string[];
  /** Current scanned sessions (read-only snapshot). */
  sessions(): SessionLite[];
  /** Project alias for a cwd, if the user set one. */
  projectAlias(cwd: string): string | null;
  /** Resume a session in a terminal (focuses an existing one if already open). */
  resume(provider: string, id: string): Promise<void>;
  /** Persisted key/value store (the extension's globalState), for Pro data. */
  getState<T>(key: string): T | undefined;
  setState(key: string, value: unknown): Promise<void>;
  /** Register a cleanup callback tied to the extension lifetime (e.g. timers). */
  addDisposable(dispose: () => void): void;
}

/** The Pro feature set. Implemented in the private `aterm-pro` module. */
export interface ProModule {
  launchParallel(api: ProApi): Promise<void>;
  compareWorktrees(api: ProApi): Promise<void>;
  cleanupWorktrees(api: ProApi): Promise<void>;
  saveTemplate(api: ProApi): Promise<void>;
  runTemplate(api: ProApi): Promise<void>;
  manageTemplates(api: ProApi): Promise<void>;
  saveWorkspaceProfile(api: ProApi): Promise<void>;
  openWorkspaceProfile(api: ProApi): Promise<void>;
  manageWorkspaceProfiles(api: ProApi): Promise<void>;
  proReport(api: ProApi): Promise<void>;
  exportConversationHtml(api: ProApi): Promise<void>;
  dailySummary(api: ProApi): Promise<void>;
  /** Optional background setup (timers/watchers). Called once when the module
   *  loads, if the gate allows Pro. */
  activate?(api: ProApi): void;
}
