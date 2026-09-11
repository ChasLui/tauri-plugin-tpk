/**
 * TypeScript API for `tauri-plugin-tpk`.
 *
 * Updates take effect on the next cold start. There is deliberately no way to
 * swap layers in a running process: the WebView would end up mixing modules
 * from two different revisions.
 *
 * The update URL and the trusted keys are native configuration. Nothing here
 * can change them — that is what keeps a scripting bug from repointing the
 * updater.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** One of the frozen error codes from the specification. */
export type ErrorCode =
  | "E_DISABLED"
  | "E_NETWORK"
  | "E_SIGNATURE"
  | "E_HASH"
  | "E_SPEC"
  | "E_PATH"
  | "E_PARENT"
  | "E_SHELL"
  | "E_WATERMARK"
  | "E_BLACKLIST"
  | "E_IO"
  | "E_DELTA"
  | "E_STATE"
  | "E_POLICY";

/** The shape a rejected command throws. */
export interface TpkError {
  code: ErrorCode;
  message: string;
}

/** A pack the channel is offering. */
export interface PackSummary {
  id: string;
  kind: "base" | "patch" | "dlc" | "mod";
  version: string;
  version_code: number;
  size: number;
}

/**
 * What {@link check} found.
 *
 * Every one of these is a normal outcome, not an exception: "you are up to
 * date" and "your app is too old for this content" are ordinary answers.
 */
export type CheckOutcome =
  | { status: "up_to_date"; watermark: number }
  | { status: "available"; packs: PackSummary[]; bytes: number; notes?: string }
  | { status: "shell_required"; min_shell: string }
  | { status: "disabled" }
  | { status: "degraded"; consecutive_rollbacks: number };

/** What {@link download} did. */
export type DownloadOutcome =
  | { status: "staged"; rev: string; bytes: number }
  | { status: "up_to_date" }
  | { status: "shell_required"; min_shell: string }
  | { status: "disabled" }
  | { status: "failed"; code: ErrorCode; message: string };

/** What {@link notifyReady} did. */
export type ReadyOutcome =
  { status: "committed"; rev: string } | { status: "noop" };

/** One loaded layer. */
export interface LayerSummary {
  id: string;
  kind: "base" | "patch" | "dlc" | "mod";
  version_code: number;
}

/** What {@link status} reports. */
export interface Status {
  /** `committed` once acknowledged, `booting` while a revision is on trial. */
  pointer: "committed" | "booting";
  rev?: string;
  layers: LayerSummary[];
  shell: string;
  watermark: number;
  /** Whether a revision is waiting for the next cold start. */
  pending: boolean;
  /** Automatic updating stops after repeated rollbacks. */
  degraded: boolean;
  /** Layers that failed to load this launch, by hash. */
  failed_layers: string[];
  /**
   * Capabilities granted to this window that overlay JavaScript can reach.
   * Empty is what you want: pack content runs on the `tauri://` origin and
   * inherits whatever the window was granted.
   */
  unsafe_capabilities: string[];
  /** Whether the binary carries a fallback for a rollback to land on. */
  has_embedded_fallback: boolean;
  last_error?: { code: ErrorCode; message: string };
}

/** Progress while downloading. */
export interface DownloadProgress {
  downloaded: number;
  total: number;
  pack_index: number;
  pack_count: number;
}

/** A state machine transition. */
export interface StateEvent {
  pointer: "staged" | "committed" | "rolled_back" | "reset";
  rev?: string;
}

/** Poll the channel for updates. */
export async function check(): Promise<CheckOutcome> {
  return await invoke("plugin:tpk|check");
}

/**
 * Download and stage whatever {@link check} found.
 *
 * The result applies on the next cold start. Tell the user that rather than
 * reloading the WebView — a reload would mix modules from two revisions.
 */
export async function download(): Promise<DownloadOutcome> {
  return await invoke("plugin:tpk|download");
}

/**
 * Acknowledge that the running revision works.
 *
 * Call it **after the first screen has actually rendered**, not at the top of
 * your entry point. What this promises is that the content is usable, and the
 * only thing that can tell is the content itself. Until it is called the
 * revision is on trial, and enough unacknowledged launches roll it back.
 *
 * Calling it more than once is harmless.
 */
export async function notifyReady(): Promise<ReadyOutcome> {
  return await invoke("plugin:tpk|notify_ready");
}

/** Report what is loaded and what is pending. */
export async function status(): Promise<Status> {
  return await invoke("plugin:tpk|status");
}

/**
 * Discard downloaded content.
 *
 * Requires the `tpk:allow-reset` capability. `clearBlacklist` also forgets
 * which releases were found to be broken, so a support agent can retry one —
 * which is exactly why it is not the default.
 */
export async function reset(options?: {
  clearBlacklist?: boolean;
}): Promise<Status> {
  return await invoke("plugin:tpk|reset", {
    options: { clear_blacklist: options?.clearBlacklist ?? false },
  });
}

/** Subscribe to download progress. */
export async function onDownloadProgress(
  handler: (progress: DownloadProgress) => void,
): Promise<UnlistenFn> {
  return await listen<DownloadProgress>("tpk://download-progress", (event) =>
    handler(event.payload),
  );
}

/** Subscribe to state machine transitions. */
export async function onState(
  handler: (event: StateEvent) => void,
): Promise<UnlistenFn> {
  return await listen<StateEvent>("tpk://state", (event) =>
    handler(event.payload),
  );
}

/** Subscribe to failures. Useful for telemetry. */
export async function onError(
  handler: (error: TpkError) => void,
): Promise<UnlistenFn> {
  return await listen<TpkError>("tpk://error", (event) =>
    handler(event.payload),
  );
}
