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
}

/** The Pro feature set. Implemented in the private `aterm-pro` module. */
export interface ProModule {
  launchParallel(api: ProApi): Promise<void>;
  compareWorktrees(api: ProApi): Promise<void>;
  cleanupWorktrees(api: ProApi): Promise<void>;
  saveTemplate(api: ProApi): Promise<void>;
  runTemplate(api: ProApi): Promise<void>;
  manageTemplates(api: ProApi): Promise<void>;
}
