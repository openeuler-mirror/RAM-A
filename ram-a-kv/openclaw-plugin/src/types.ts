export interface RamAkvEventResponse {
  ok: boolean;
  type: string;
  data?: Record<string, unknown>;
  error?: string;
}

export interface TurnStartResult {
  prefetch_sent: boolean;
  prefetch_count: number;
}

export interface TurnEndResult {
  evicted_count: number;
  map_updated: boolean;
  debug_written: boolean;
}

export interface SnapshotRestoreResult {
  prefetch_sent: boolean;
  prefetch_count: number;
}

export interface SessionMapResult {
  session_id: string;
  chunk_hashes: string[];
}

export interface SessionCloseResult {
  evicted_count: number;
  closed: boolean;
}

export interface SessionSuspendResult {
  evicted_count: number;
  suspended: boolean;
}

export interface HealthResult {
  status: string;
  sessions_count: number;
}

export interface DebugContext {
  messages?: unknown[];
  timing?: {
    ttft_ms?: number;
    total_time_ms?: number;
    tpot_ms?: number;
  };
}

export interface RamAkvPluginConfig {
  daemonUrl: string;
  authToken: string;
  restoreOnSessionStart: boolean;
  skipRestoreReasons: string[];
  prefetchOnTurnStart: boolean;
  requestTimeoutMs: number;
}

export interface KvTransferParams {
  chunk_hashes: string[];
}

export type AssistantMessageEvent =
  | { type: "start"; partial: AssistantMessage }
  | { type: "text_start"; contentIndex: number; partial: AssistantMessage }
  | { type: "text_delta"; contentIndex: number; delta: string; partial?: AssistantMessage }
  | { type: "text_end"; contentIndex: number; content: string; partial: AssistantMessage }
  | { type: "thinking_start"; contentIndex: number; partial: AssistantMessage }
  | { type: "thinking_delta"; contentIndex: number; delta: string; partial?: AssistantMessage }
  | { type: "thinking_end"; contentIndex: number; content: string; partial: AssistantMessage }
  | { type: "toolcall_start"; contentIndex: number; partial: AssistantMessage }
  | { type: "toolcall_delta"; contentIndex: number; delta: string; partial?: AssistantMessage }
  | { type: "toolcall_end"; contentIndex: number; toolCall: ToolCall; partial: AssistantMessage }
  | { type: "done"; reason: "stop" | "length" | "toolUse"; message: AssistantMessage }
  | { type: "error"; reason: "error" | "aborted"; error: AssistantMessage };

export interface ToolCall {
  type: "toolCall";
  id: string;
  name: string;
  arguments: Record<string, unknown>;
}

export interface AssistantMessageEventStream {
  [Symbol.asyncIterator](): AsyncIterator<AssistantMessageEvent>;
  result(): Promise<AssistantMessage>;
}

export interface StreamOptions {
  sessionId?: string;
  requestId?: string;
  signal?: AbortSignal;
  apiKey?: string;
  onPayload?: (payload: unknown, model: ProviderRuntimeModel) => unknown | Promise<unknown>;
  onResponse?: (response: { status: number; headers: Record<string, string> }, model: ProviderRuntimeModel) => void | Promise<void>;
}

export interface AgentMessage {
  role: string;
  content?: string | unknown[];
}

export interface StreamContext {
  messages: AgentMessage[];
  systemPrompt?: string;
  tools?: unknown[];
}

export interface ProviderRuntimeModel {
  id: string;
  name?: string;
  provider: string;
  api?: string;
  baseUrl?: string;
  contextWindow?: number;
  maxTokens?: number;
}

export interface AssistantMessage {
  role: "assistant";
  content: unknown[];
  api?: string;
  provider?: string;
  model?: string;
  responseModel?: string;
  responseId?: string;
  usage?: {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
    totalTokens: number;
  };
  stopReason?: string;
  timestamp: number;
}

export interface ChatCompletionDelta {
  content?: string | null;
  role?: string;
  reasoning_content?: string | null;
  reasoning?: string | null;
  tool_calls?: Array<{
    index?: number;
    id?: string;
    function?: { name?: string; arguments?: string };
  }>;
}

export interface ChatCompletionChunk {
  id: string;
  object: string;
  created: number;
  model: string;
  choices: Array<{
    index: number;
    delta: ChatCompletionDelta;
    finish_reason: string | null;
  }>;
  usage?: {
    prompt_tokens?: number;
    completion_tokens?: number;
    total_tokens?: number;
    prompt_tokens_details?: { cached_tokens?: number };
  } | null;
  kv_transfer_params?: KvTransferParams | null;
}
