import { definePluginEntry } from "openclaw/plugin-sdk/plugin-entry";
import type { StreamFn } from "openclaw/plugin-sdk/plugin-entry";
import { RamAkvClient } from "./ram-akv-client.js";
import { ChunkStore } from "./chunk-store.js";
import { createStreamTransport } from "./stream-transport.js";
import type {
  RamAkvPluginConfig,
  ProviderRuntimeModel,
  StreamContext,
  StreamOptions,
  AssistantMessageEventStream,
  ChatCompletionChunk,
  AssistantMessage,
  KvTransferParams,
} from "./types.js";

const DEFAULT_CONFIG: RamAkvPluginConfig = {
  daemonUrl: "http://127.0.0.1:6998",
  authToken: "",
  restoreOnSessionStart: true,
  skipRestoreReasons: ["new", "reset", "deleted"],
  prefetchOnTurnStart: true,
  requestTimeoutMs: 6000,
};

function resolveConfig(
  raw: Record<string, unknown> | undefined,
): RamAkvPluginConfig {
  if (!raw) return DEFAULT_CONFIG;
  return {
    daemonUrl: typeof raw.daemonUrl === "string" ? raw.daemonUrl : DEFAULT_CONFIG.daemonUrl,
    authToken: typeof raw.authToken === "string" ? raw.authToken : DEFAULT_CONFIG.authToken,
    restoreOnSessionStart:
      typeof raw.restoreOnSessionStart === "boolean"
        ? raw.restoreOnSessionStart
        : DEFAULT_CONFIG.restoreOnSessionStart,
    skipRestoreReasons: Array.isArray(raw.skipRestoreReasons)
      ? raw.skipRestoreReasons.map(String)
      : DEFAULT_CONFIG.skipRestoreReasons,
    prefetchOnTurnStart:
      typeof raw.prefetchOnTurnStart === "boolean"
        ? raw.prefetchOnTurnStart
        : DEFAULT_CONFIG.prefetchOnTurnStart,
    requestTimeoutMs:
      typeof raw.requestTimeoutMs === "number"
        ? raw.requestTimeoutMs
        : DEFAULT_CONFIG.requestTimeoutMs,
  };
}

const STRIP_MSG_KEYS = new Set([
  "__messages", "__timing", "kvTransferParams",
  "api", "provider", "model", "responseId", "responseModel",
  "stopReason", "usage", "__openclaw", "idempotencyKey",
]);

function stripMessagesForDebug(msgs: unknown[]): unknown[] {
  return msgs.map((m) => {
    if (!m || typeof m !== "object") return m;
    const src = m as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(src)) {
      if (!STRIP_MSG_KEYS.has(k)) out[k] = src[k];
    }
    return out;
  });
}

interface ProviderModelConfig {
  baseUrl: string;
  apiKey: string;
}

function resolveProviderConfig(
  openclawConfig: unknown,
  providerId: string,
): ProviderModelConfig {
  const cfg = openclawConfig as {
    models?: {
      providers?: Record<string, { baseUrl?: string; apiKey?: string }>;
    };
  };
  const provider = cfg?.models?.providers?.[providerId];
  return {
    baseUrl: provider?.baseUrl ?? "http://localhost:8000/v1",
    apiKey: provider?.apiKey ?? "",
  };
}

export default definePluginEntry({
  id: "ram-a-kv-hook",
  name: "ram-a-kv KV Cache",
  description:
    "Provider plugin that intercepts LLM responses for KV cache lifecycle management with ram-a-kv daemon",
  register(api) {
    const config = resolveConfig(
      api.pluginConfig as Record<string, unknown> | undefined,
    );
    const client = new RamAkvClient(config.daemonUrl, config.requestTimeoutMs, config.authToken);
    const chunkStore = new ChunkStore();

    try {
      chunkStore.init(api as never);
    } catch {
      // State store may not be available for external plugins
    }

    const log = api.logger;

    const g = globalThis as { __ramAkvHookSessionMap?: Map<string, string> };
    g.__ramAkvHookSessionMap ??= new Map<string, string>();
    const sessionKeyToId = g.__ramAkvHookSessionMap;

    function resolveSessionId(sessionKey: string): string | undefined {
      return sessionKeyToId.get(sessionKey);
    }

    api.registerProvider({
      id: "ram-a-kv",
      label: "ram-a-kv KV Cache Provider",
      auth: [],
      createStreamFn: (ctx) => {
        const providerConfig = resolveProviderConfig(ctx.config, "ram-a-kv");

        const wrappedStreamFn = async (
          model: ProviderRuntimeModel,
          context: StreamContext,
          options?: StreamOptions,
        ): Promise<AssistantMessageEventStream> => {
          const sessionId = options?.sessionId;

          if (sessionId && config.prefetchOnTurnStart) {
            try {
              const result = await client.turnStart(sessionId);
              log.info(`ram-a-kv turn_start: ${sessionId} (prefetch_sent=${result.prefetch_sent}, count=${result.prefetch_count})`);
            } catch (err) {
              log.warn(`ram-a-kv turn_start failed for ${sessionId}: ${err instanceof Error ? err.message : String(err)}`);
            }
          }

          const streamFn = createStreamTransport(
            {
              baseUrl: providerConfig.baseUrl,
              apiKey: providerConfig.apiKey,
              timeoutMs: 300000,
            },
            {
              onChunk: (chunk: ChatCompletionChunk) => {
                // chunk-level inspection — kv_transfer_params is captured internally
              },
              onDebug: (msg: string) => {
                log.info(`[ram-a-kv-debug] ${msg}`);
              },
              onDone: async (message: AssistantMessage, debug?: { messages?: unknown[]; timing?: { totalMs: number; ttftMs: number } }) => {
                const timing = debug?.timing;
                if (timing) {
                  log.info(`[ram-a-kv-hook] inference timing: total=${timing.totalMs}ms, ttft=${timing.ttftMs}ms`);
                }
                log.info(`[ram-a-kv-hook] onDone entered, sessionId=${sessionId}`);
                const rawKvTransferParams = (message as unknown as Record<string, unknown>).kvTransferParams;
                const chunkHashes = (rawKvTransferParams as KvTransferParams | undefined)?.chunk_hashes ?? [];

                // Build debug context from callback params (not from message side-effects)
                const debugContext = (debug?.messages || timing) ? {
                  messages: debug?.messages ? stripMessagesForDebug(debug.messages) : undefined,
                  timing: timing ? {
                    ttft_ms: timing.ttftMs,
                    total_time_ms: timing.totalMs,
                    tpot_ms: undefined,
                  } : undefined,
                } : undefined;

                if (sessionId) {
                  log.info(`[ram-a-kv-hook] onDone calling turnEnd, hashes=${chunkHashes.length}, rawType=${typeof rawKvTransferParams}, debugContext=${!!debugContext}`);
                  try {
                    await client.turnEnd(sessionId, chunkHashes, debugContext, rawKvTransferParams);
                    log.info(`ram-a-kv turn_end: ${sessionId} (${chunkHashes.length} chunk_hashes)`);
                  } catch (err) {
                    log.warn(`ram-a-kv turn_end failed for ${sessionId}: ${err instanceof Error ? err.message : String(err)}`);
                  }
                  try {
                    await chunkStore.saveChunkHashes(sessionId, chunkHashes);
                  } catch (err) {
                    log.warn(`ram-a-kv chunkStore save failed for ${sessionId}: ${err instanceof Error ? err.message : String(err)}`);
                  }
                }
              },
              onError: (error: Error) => {
                log.warn(`ram-a-kv transport error: ${error.message}\n${error.stack ?? ""}`);
              },
            },
          );

          return streamFn(model, context, options);
        };

        return wrappedStreamFn as unknown as StreamFn;
      },
    });

    api.on("session_end", async (event: { sessionId: string; sessionKey?: string; reason?: string; nextSessionId?: string; nextSessionKey?: string }) => {
      const sessionId = event.sessionId;
      const sessionKey = event.sessionKey;
      const reason = event.reason ?? "unknown";
      const nextSessionId = event.nextSessionId;
      const nextSessionKey = event.nextSessionKey;

      if (sessionKey) {
        sessionKeyToId.delete(sessionKey);
      } else if (sessionId) {
        // Fallback: when sessionKey is missing (some session_end emitters
        // omit it), scan and remove any entry whose id matches sessionId so
        // the global map does not grow unbounded over long daemon runs.
        for (const [k, v] of sessionKeyToId.entries()) {
          if (v === sessionId) {
            sessionKeyToId.delete(k);
          }
        }
      }
      if (nextSessionKey && nextSessionId) {
        sessionKeyToId.set(nextSessionKey, nextSessionId);
      }
      if (nextSessionId) {
        try { await chunkStore.saveReason(nextSessionId, reason); } catch { /* ignore */ }
      }

      const terminalReasons = ["deleted"];
      if (!nextSessionId && terminalReasons.includes(reason)) {
        try {
          await client.sessionClose(sessionId);
          log.info(`ram-a-kv session_close: ${sessionId} (reason=${reason})`);
        } catch (err) {
          log.warn(`ram-a-kv session_close failed for ${sessionId}: ${err instanceof Error ? err.message : String(err)}`);
        }
        try { await chunkStore.remove(sessionId); } catch { /* ignore */ }
      } else {
        try {
          await client.sessionSuspend(sessionId);
          log.info(`ram-a-kv session_suspend: ${sessionId} (reason=${reason})`);
        } catch (err) {
          log.warn(`ram-a-kv session_suspend failed for ${sessionId}: ${err instanceof Error ? err.message : String(err)}`);
        }
      }
    });

    api.on("session_start", async (event: { sessionId: string; sessionKey?: string; resumedFrom?: string }) => {
      const sessionId = event.sessionId;
      const sessionKey = event.sessionKey;
      if (sessionKey) {
        sessionKeyToId.set(sessionKey, sessionId);
      }
      if (!config.restoreOnSessionStart) return;
      if (!event.resumedFrom) return;

      const resumedFrom = event.resumedFrom;
      let reason: string | undefined;
      try { reason = await chunkStore.consumeReason(sessionId); } catch { /* ignore */ }

      if (reason && config.skipRestoreReasons.includes(reason)) {
        log.info(`ram-a-kv skip snapshot_restore: ${sessionId} (reason=${reason} is in skip list)`);
        return;
      }

      try {
        const result = await client.snapshotRestore(sessionId, resumedFrom);
        log.info(`ram-a-kv snapshot_restore: ${sessionId} from ${resumedFrom} (prefetch_sent=${result.prefetch_sent}, count=${result.prefetch_count})`);
      } catch (err) {
        log.warn(`ram-a-kv snapshot_restore failed for ${sessionId}: ${err instanceof Error ? err.message : String(err)}`);
      }
    });

    api.registerHttpRoute({
      path: "/api/ram-a-kv/switch",
      auth: "gateway",
      match: "exact",
      gatewayRuntimeScopeSurface: "trusted-operator",
      handler: async (req: unknown, res: unknown) => {
        try {
          const r = req as { body?: unknown; on?: (event: string, cb: (chunk: unknown) => void) => void };
          let bodyText = "";
          if (typeof r.body === "string") {
            bodyText = r.body;
          } else if (r.body && typeof r.body === "object") {
            bodyText = JSON.stringify(r.body);
          } else if (r.on) {
            // Cap the streamed body so a malicious or buggy client cannot
            // exhaust memory by streaming gigabytes of data.
            const MAX_SWITCH_BODY_CHARS = 1 * 1024 * 1024; // 1 MiB
            await new Promise<void>((resolve, reject) => {
              r.on!("data", (chunk: unknown) => {
                bodyText += String(chunk);
                if (bodyText.length > MAX_SWITCH_BODY_CHARS) {
                  reject(new Error(`switch body exceeds ${MAX_SWITCH_BODY_CHARS} bytes`));
                }
              });
              r.on!("end", () => { resolve(); });
              r.on!("error", (err: unknown) => {
                reject(err instanceof Error ? err : new Error(String(err)));
              });
            }).catch((err) => {
              const s = res as { setHeader?: (k: string, v: string) => void; end?: (s: string) => void; statusCode?: number };
              s.statusCode = 413;
              s.setHeader?.("Content-Type", "application/json");
              s.end?.(JSON.stringify({ ok: false, error: err instanceof Error ? err.message : String(err) }));
              throw new Error("body_too_large");
            });
          }
          const body = JSON.parse(bodyText || "{}") as { sessionKey?: string };
          const sessionKey = body.sessionKey;
          if (!sessionKey) {
            const s = res as { setHeader?: (k: string, v: string) => void; end?: (s: string) => void; statusCode?: number };
            s.statusCode = 400;
            s.setHeader?.("Content-Type", "application/json");
            s.end?.(JSON.stringify({ ok: false, error: "missing sessionKey" }));
            return;
          }
          const sessionId = resolveSessionId(sessionKey) ?? sessionKey;
          log.info(`[ram-a-kv-hook] switch: sessionKey=${sessionKey}, resolved=${sessionId}, mapSize=${sessionKeyToId.size}, mapKeys=${[...sessionKeyToId.keys()].join(",")}`);
          if (!config.restoreOnSessionStart) {
            const s = res as { setHeader?: (k: string, v: string) => void; end?: (s: string) => void; statusCode?: number };
            s.setHeader?.("Content-Type", "application/json");
            s.end?.(JSON.stringify({ ok: true, skipped: true }));
            return;
          }
          // snapshot_restore for a switch always sources from the same session id
          // (no resume). source_session_id is omitted so the daemon falls back
          // to session_id.
          const result = await client.snapshotRestore(sessionId);
          log.info(`ram-a-kv switch prefetch: ${sessionId} (prefetch_sent=${result.prefetch_sent}, count=${result.prefetch_count})`);
          const s = res as { setHeader?: (k: string, v: string) => void; end?: (s: string) => void; statusCode?: number };
          s.setHeader?.("Content-Type", "application/json");
          s.end?.(JSON.stringify({ ok: true, prefetch_sent: result.prefetch_sent, prefetch_count: result.prefetch_count }));
        } catch (err) {
          if (err instanceof Error && err.message === "body_too_large") {
            return;
          }
          log.warn(`ram-a-kv switch prefetch failed: ${err instanceof Error ? err.message : String(err)}`);
          const s = res as { setHeader?: (k: string, v: string) => void; end?: (s: string) => void; statusCode?: number };
          s.statusCode = 500;
          s.setHeader?.("Content-Type", "application/json");
          s.end?.(JSON.stringify({ ok: false, error: err instanceof Error ? err.message : String(err) }));
        }
      },
    });

    api.on("before_agent_run", async (event: unknown, ctx: unknown) => {
      // Register the sessionKey -> sessionId mapping before the agent actually
      // runs so that subsequent switch / session_end hooks can resolve the id.
      const e = (event ?? {}) as { sessionId?: string; sessionKey?: string };
      const c = (ctx ?? {}) as { sessionId?: string; sessionKey?: string };
      const sessionId = e.sessionId ?? c.sessionId;
      const sessionKey = e.sessionKey ?? c.sessionKey;
      log.info(`[ram-a-kv-hook] before_agent_run: sessionKey=${sessionKey}, sessionId=${sessionId}`);
      if (sessionKey && sessionId) {
        sessionKeyToId.set(sessionKey, sessionId);
      }
    });

    api.on("gateway_start", async () => {
      try {
        const health = await client.health();
        log.info(`ram-a-kv daemon connected: status=${health.status}, sessions=${health.sessions_count}`);
      } catch (err) {
        log.warn(`ram-a-kv daemon not reachable at ${config.daemonUrl}: ${err instanceof Error ? err.message : String(err)}`);
      }
    });

    api.on("gateway_stop", async () => {
      log.info("ram-a-kv hook: gateway stopping");
    });
  },
});
