import type {
  RamAkvEventResponse,
  TurnStartResult,
  TurnEndResult,
  SnapshotRestoreResult,
  SessionMapResult,
  SessionCloseResult,
  SessionSuspendResult,
  HealthResult,
  DebugContext,
} from "./types.js";

export class RamAkvClient {
  private readonly baseUrl: string;
  private readonly timeoutMs: number;
  private readonly authToken: string;

  constructor(baseUrl: string, timeoutMs = 6000, authToken = "") {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.timeoutMs = timeoutMs;
    this.authToken = authToken;
  }

  private async sendEvent(
    type: string,
    payload: Record<string, unknown>,
  ): Promise<RamAkvEventResponse> {
    const body = { type, ...payload };
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const headers: Record<string, string> = { "Content-Type": "application/json" };
      if (this.authToken) {
        headers["Authorization"] = `Bearer ${this.authToken}`;
      }
      const res = await fetch(`${this.baseUrl}/event`, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      const json = (await res.json()) as RamAkvEventResponse;
      if (!json.ok) {
        throw new Error(
          `ram-a-kv event "${type}" failed: ${json.error ?? "unknown error"}`,
        );
      }
      return json;
    } finally {
      clearTimeout(timer);
    }
  }

  async turnStart(sessionId: string): Promise<TurnStartResult> {
    const res = await this.sendEvent("turn_start", { session_id: sessionId });
    return (res.data ?? {}) as unknown as TurnStartResult;
  }

  async turnEnd(
    sessionId: string,
    chunkHashes: string[],
    debugContext?: DebugContext,
    rawKvTransferParams?: unknown,
  ): Promise<TurnEndResult> {
    const payload: Record<string, unknown> = {
      session_id: sessionId,
      chunk_hashes: chunkHashes,
      kv_transfer_params: rawKvTransferParams ?? { chunk_hashes: chunkHashes },
    };
    if (debugContext) {
      payload.debug_context = debugContext;
    }
    const res = await this.sendEvent("turn_end", payload);
    return (res.data ?? {}) as unknown as TurnEndResult;
  }

  async snapshotRestore(
    sessionId: string,
    sourceSessionId?: string,
  ): Promise<SnapshotRestoreResult> {
    const payload: Record<string, unknown> = { session_id: sessionId };
    if (sourceSessionId) {
      payload.source_session_id = sourceSessionId;
    }
    const res = await this.sendEvent("snapshot_restore", payload);
    return (res.data ?? {}) as unknown as SnapshotRestoreResult;
  }

  async sessionSuspend(sessionId: string): Promise<SessionSuspendResult> {
    const res = await this.sendEvent("session_suspend", {
      session_id: sessionId,
    });
    return (res.data ?? {}) as unknown as SessionSuspendResult;
  }

  async sessionMap(sessionId: string): Promise<SessionMapResult> {
    const res = await this.sendEvent("session_map", {
      session_id: sessionId,
    });
    return (res.data ?? {}) as unknown as SessionMapResult;
  }

  async sessionClose(sessionId: string): Promise<SessionCloseResult> {
    const res = await this.sendEvent("session_close", {
      session_id: sessionId,
    });
    return (res.data ?? {}) as unknown as SessionCloseResult;
  }

  async health(): Promise<HealthResult> {
    const res = await this.sendEvent("health", {});
    return (res.data ?? {}) as unknown as HealthResult;
  }
}
