import type {
  AssistantMessageEvent,
  AssistantMessageEventStream,
  AssistantMessage,
  ChatCompletionChunk,
  ChatCompletionDelta,
  StreamContext,
  StreamOptions,
  ProviderRuntimeModel,
  KvTransferParams,
  ToolCall,
} from "./types.js";

export interface TurnDebugInfo {
  messages?: unknown[];
  timing?: { totalMs: number; ttftMs: number };
}

interface StreamCallbacks {
  onChunk?: (chunk: ChatCompletionChunk) => void;
  onDone?: (message: AssistantMessage, debug?: TurnDebugInfo) => void | Promise<void>;
  onError?: (error: Error) => void;
  onDebug?: (message: string) => void;
}

interface ProviderTransportConfig {
  baseUrl: string;
  apiKey: string;
  timeoutMs: number;
}

function mapStopReason(finishReason: string | null): "stop" | "length" | "toolUse" {
  if (!finishReason) return "stop";
  switch (finishReason) {
    case "stop": return "stop";
    case "length": return "length";
    case "tool_calls":
    case "function_call":
      return "toolUse";
    default: return "stop";
  }
}

// Parse a possibly-incomplete JSON string into an object. Used so streaming
// tool-call argument deltas are reflected in `partial.content[].arguments`
// even before the full JSON has arrived. Returns {} on parse failure so the
// finalMessage always carries a valid arguments object.
function parsePartialJson(text: string): Record<string, unknown> {
  const trimmed = text.trim();
  if (!trimmed) return {};
  try {
    const parsed = JSON.parse(trimmed);
    return (parsed && typeof parsed === "object" && !Array.isArray(parsed))
      ? parsed as Record<string, unknown>
      : {};
  } catch {
    return {};
  }
}

function createPartialAssistantMessage(model: ProviderRuntimeModel): AssistantMessage {
  return {
    role: "assistant",
    content: [],
    api: model.api,
    provider: model.provider,
    model: model.id,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0 },
    stopReason: "stop",
    timestamp: Date.now(),
  };
}

function clonePartial(partial: AssistantMessage): AssistantMessage {
  return {
    ...partial,
    content: [...partial.content],
    usage: { ...partial.usage! },
  };
}

async function* iterateSSE(
  body: ReadableStream<Uint8Array>,
  signal?: AbortSignal,
): AsyncGenerator<ChatCompletionChunk> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  try {
    while (true) {
      if (signal?.aborted) {
        throw new DOMException("Aborted", "AbortError");
      }
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      let idx: number;
      while ((idx = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, idx);
        buffer = buffer.slice(idx + 1);
        const trimmed = line.trim();
        if (!trimmed || trimmed.startsWith(":")) continue;
        if (!trimmed.startsWith("data:")) continue;
        const data = trimmed.slice(5).trim();
        if (data === "[DONE]") return;
        try {
          yield JSON.parse(data) as ChatCompletionChunk;
        } catch {
          // Skip malformed JSON
        }
      }
    }
  } finally {
    reader.releaseLock();
  }
}

function formatTools(tools: unknown[]): unknown[] {
  const result: unknown[] = [];
  for (const tool of tools) {
    if (!tool || typeof tool !== "object") continue;
    const t = tool as Record<string, unknown>;
    if (t.type === "function" && t.function && typeof t.function === "object") {
      result.push(t);
    } else if (t.name && typeof t.name === "string") {
      result.push({
        type: "function",
        function: {
          name: t.name,
          description: t.description ?? "",
          parameters: t.parameters ?? { type: "object", properties: {} },
        },
      });
    }
  }
  return result;
}

function buildRequestBody(
  model: ProviderRuntimeModel,
  context: StreamContext,
  _options?: StreamOptions,
): Record<string, unknown> {
  const messages: Array<Record<string, unknown>> = [];
  if (context.systemPrompt) {
    messages.push({ role: "system", content: context.systemPrompt });
  }
  for (const msg of context.messages) {
    const role = msg.role === "toolResult" ? "tool" : msg.role;
    const rawContent = msg.content;
    if (typeof rawContent === "string") {
      messages.push({ role, content: rawContent });
    } else if (Array.isArray(rawContent)) {
      const textParts: string[] = [];
      const toolCalls: Array<Record<string, unknown>> = [];
      for (const block of rawContent) {
        if (!block || typeof block !== "object") continue;
        const b = block as Record<string, unknown>;
        if (b.type === "text" && typeof b.text === "string") {
          textParts.push(b.text);
        } else if (b.type === "toolCall") {
          toolCalls.push({
            id: b.id ?? "",
            type: "function",
            function: {
              name: b.name ?? "",
              arguments: typeof b.arguments === "string"
                ? b.arguments
                : JSON.stringify(b.arguments ?? {}),
            },
          });
        } else if (b.type === "toolResult") {
          const toolContent = typeof b.content === "string"
            ? b.content
            : typeof b.content === "object" && b.content !== null
              ? JSON.stringify(b.content)
              : "";
          messages.push({
            role: "tool",
            tool_call_id: b.toolCallId ?? b.id ?? "",
            content: toolContent,
          });
          continue;
        }
      }
      if (textParts.length > 0 || toolCalls.length > 0) {
        const message: Record<string, unknown> = { role };
        if (textParts.length > 0) {
          message.content = textParts.join("");
        }
        if (toolCalls.length > 0) {
          message.tool_calls = toolCalls;
        }
        if (!message.content && toolCalls.length === 0) {
          message.content = "";
        }
        messages.push(message);
      }
    } else {
      // Fallback for any non-string, non-array content: emit an empty string so
      // the message shape stays valid for the model API. (A previous duplicate
      // `typeof rawContent === "string"` branch here was unreachable.)
      messages.push({ role, content: "" });
    }
  }
  const body: Record<string, unknown> = {
    model: model.id,
    messages,
    stream: true,
    stream_options: { include_usage: true },
  };
  if (context.tools && context.tools.length > 0) {
    const formatted = formatTools(context.tools);
    if (formatted.length > 0) {
      body.tools = formatted;
    }
  }
  return body;
}

export function createStreamTransport(
  transportConfig: ProviderTransportConfig,
  callbacks: StreamCallbacks,
): (model: ProviderRuntimeModel, context: StreamContext, options?: StreamOptions) => Promise<AssistantMessageEventStream> {
  const { baseUrl, apiKey, timeoutMs } = transportConfig;

  return async (model, context, options) => {
    const signal = options?.signal;
    const t0 = Date.now();
    let firstTokenTime = 0;
    let fetchStart = 0;
    let body = buildRequestBody(model, context, options);
    const t1 = Date.now();
    const bodySize = JSON.stringify(body).length;
    const toolCount = Array.isArray(body.tools) ? body.tools.length : 0;
    const messagesSize = JSON.stringify(body.messages).length;
    callbacks.onDebug?.(`buildRequestBody: ${t1 - t0}ms, messages count: ${(body.messages as unknown[])?.length ?? 0}, bodySize: ${bodySize} bytes, messagesSize: ${messagesSize} bytes, tools: ${toolCount}`);
    if (options?.onPayload) {
      const patched = await options.onPayload(body, model);
      if (patched && typeof patched === "object") {
        body = patched as Record<string, unknown>;
      }
    }
    const t2 = Date.now();
    callbacks.onDebug?.(`onPayload: ${t2 - t1}ms`);

    const bodyStr = JSON.stringify(body);
    if (bodyStr.includes("toolCall")) {
      callbacks.onDebug?.(`WARNING: toolCall found in body after onPayload! preview: ${bodyStr.slice(0, 500)}`);
    }

    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), timeoutMs);
    if (signal) {
      signal.addEventListener("abort", () => controller.abort());
    }

    fetchStart = Date.now();
    let response: Response;
    try {
      response = await fetch(`${baseUrl}/chat/completions`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${apiKey}`,
        },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
    } catch (err) {
      clearTimeout(timeout);
      const error = err instanceof Error ? err : new Error(String(err));
      callbacks.onError?.(error);
      throw error;
    }
    const t3 = Date.now();
    callbacks.onDebug?.(`fetch response: ${t3 - t2}ms (HTTP ${response.status})`);

    if (options?.onResponse) {
      const headers: Record<string, string> = {};
      response.headers.forEach((value, key) => {
        headers[key] = value;
      });
      await options.onResponse({ status: response.status, headers }, model);
    }
    const t4 = Date.now();
    callbacks.onDebug?.(`onResponse: ${t4 - t3}ms`);

    if (!response.ok || !response.body) {
      clearTimeout(timeout);
      const errorText = await response.text().catch(() => "unknown error");
      const error = new Error(`LLM request failed: ${response.status} ${errorText}`);
      callbacks.onError?.(error);
      throw error;
    }
    const t5 = Date.now();
    callbacks.onDebug?.(`response body check: ${t5 - t4}ms`);

    const sseIterator = iterateSSE(response.body, controller.signal);

    let resultResolve: ((value: AssistantMessage) => void) | null = null;
    const resultPromise = new Promise<AssistantMessage>((resolve) => {
      resultResolve = resolve;
    });

    const self: AssistantMessageEventStream = {
      [Symbol.asyncIterator]() {
        const iterator = sseIterator[Symbol.asyncIterator]();
        const partial = createPartialAssistantMessage(model);
        const eventQueue: AssistantMessageEvent[] = [];
        let started = false;
        let textContentIndex = -1;
        let thinkingContentIndex = -1;
        const toolCallContentIndices = new Map<number, number>();
        let nextContentIndex = 0;
        let fullText = "";
        const MAX_RESPONSE_CHARS = 2000000;
        let responseTruncated = false;
        let kvTransferParams: KvTransferParams | null = null;
        let responseId = "";
        let responseModel = model.id;
        let stopReason: "stop" | "length" | "toolUse" = "stop";
        let usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0 };
        const toolCallBuffers = new Map<number, { id: string; name: string; args: string }>();
        let streamFinished = false;
        let turnTiming: { totalMs: number; ttftMs: number } | undefined;

        function processChunk(chunk: ChatCompletionChunk): void {
          if (chunk.id) responseId = chunk.id;
          if (chunk.model) responseModel = chunk.model;

          if (chunk.kv_transfer_params) {
            kvTransferParams = chunk.kv_transfer_params;
          }

          if (chunk.usage) {
            const u = chunk.usage;
            usage = {
              input: u.prompt_tokens ?? 0,
              output: u.completion_tokens ?? 0,
              cacheRead: u.prompt_tokens_details?.cached_tokens ?? 0,
              cacheWrite: 0,
              totalTokens: u.total_tokens ?? 0,
            };
            partial.usage = { ...usage };
          }

          const choice = chunk.choices?.[0];
          if (!choice) return;

          if (firstTokenTime === 0) {
            firstTokenTime = Date.now();
          }

          if (choice.finish_reason) {
            stopReason = mapStopReason(choice.finish_reason);
            partial.stopReason = stopReason;
          }

          if (!started) {
            started = true;
            eventQueue.push({ type: "start", partial: clonePartial(partial) });
          }

          const delta: ChatCompletionDelta = choice.delta ?? {};

          if (delta.content) {
            if (textContentIndex < 0) {
              textContentIndex = nextContentIndex++;
              partial.content.push({ type: "text", text: "" });
              eventQueue.push({ type: "text_start", contentIndex: textContentIndex, partial: clonePartial(partial) });
            }
            const textBlock = partial.content[textContentIndex] as { type: string; text: string };
            // Once the response is truncated, stop accumulating AND stop emitting
            // text_delta events so the published event stream matches the
            // internally stored text (previously deltas were still emitted
            // even though fullText/textBlock.text stopped growing, leaving
            // consumers with an inconsistent view of the assistant message).
            if (!responseTruncated) {
              textBlock.text += delta.content;
              fullText += delta.content;
              if (fullText.length > MAX_RESPONSE_CHARS) {
                responseTruncated = true;
                callbacks.onDebug?.(`Response truncated at ${MAX_RESPONSE_CHARS} chars`);
              }
              if (!responseTruncated) {
                eventQueue.push({ type: "text_delta", contentIndex: textContentIndex, delta: delta.content, partial: clonePartial(partial) });
              }
            }
          }

          const reasoningText = delta.reasoning_content ?? delta.reasoning;
          if (reasoningText) {
            if (thinkingContentIndex < 0) {
              thinkingContentIndex = nextContentIndex++;
              partial.content.push({ type: "thinking", thinking: "" });
              eventQueue.push({ type: "thinking_start", contentIndex: thinkingContentIndex, partial: clonePartial(partial) });
            }
            const thinkingBlock = partial.content[thinkingContentIndex] as { type: string; thinking: string };
            thinkingBlock.thinking += reasoningText;
            eventQueue.push({ type: "thinking_delta", contentIndex: thinkingContentIndex, delta: reasoningText, partial: clonePartial(partial) });
          }

          if (delta.tool_calls) {
            for (const tc of delta.tool_calls) {
              const tcIndex = tc.index ?? 0;
              if (!toolCallContentIndices.has(tcIndex)) {
                const ci = nextContentIndex++;
                toolCallContentIndices.set(tcIndex, ci);
                const buf = { id: tc.id ?? "", name: tc.function?.name ?? "", args: "" };
                toolCallBuffers.set(tcIndex, buf);
                partial.content.push({ type: "toolCall", id: buf.id, name: buf.name, arguments: {} });
                eventQueue.push({ type: "toolcall_start", contentIndex: ci, partial: clonePartial(partial) });
              }
              if (tc.function?.arguments) {
                const ci = toolCallContentIndices.get(tcIndex)!;
                const buf = toolCallBuffers.get(tcIndex)!;
                buf.args += tc.function.arguments;
                // Maintain the parsed arguments on the partial content block so
                // finalMessage carries the real tool call arguments. Without this,
                // the next request to the model would re-serialize {} and break
                // multi-turn tool-call conversations.
                const block = partial.content[ci] as {
                  type: string;
                  id: string;
                  name: string;
                  arguments: Record<string, unknown>;
                };
                block.arguments = parsePartialJson(buf.args);
                eventQueue.push({ type: "toolcall_delta", contentIndex: ci, delta: tc.function.arguments, partial: clonePartial(partial) });
              }
            }
          }
        }

        // Emit `text_end` / `thinking_end` / `toolcall_end` for the active
        // content blocks before the done event. The previous implementation
        // declared these events in AssistantMessageEvent but never emitted
        // them, leaving consumers unable to react to block lifecycle closure
        // (toolcall_end is also the natural place to read the final arguments).
        function emitBlockEnds(finalPartial: AssistantMessage): void {
          if (textContentIndex >= 0) {
            const block = finalPartial.content[textContentIndex] as { type: string; text: string } | undefined;
            if (block?.type === "text") {
              eventQueue.push({
                type: "text_end",
                contentIndex: textContentIndex,
                content: block.text,
                partial: clonePartial(finalPartial),
              });
            }
          }
          if (thinkingContentIndex >= 0) {
            const block = finalPartial.content[thinkingContentIndex] as { type: string; thinking: string } | undefined;
            if (block?.type === "thinking") {
              eventQueue.push({
                type: "thinking_end",
                contentIndex: thinkingContentIndex,
                content: block.thinking,
                partial: clonePartial(finalPartial),
              });
            }
          }
          for (const [tcIndex, ci] of toolCallContentIndices) {
            const block = finalPartial.content[ci] as
              | { type: "toolCall"; id: string; name: string; arguments: Record<string, unknown> }
              | undefined;
            if (block?.type !== "toolCall") continue;
            eventQueue.push({
              type: "toolcall_end",
              contentIndex: ci,
              toolCall: { type: "toolCall", id: block.id, name: block.name, arguments: block.arguments ?? {} },
              partial: clonePartial(finalPartial),
            });
            // Track that we already emitted the end event for this block.
            void tcIndex;
          }
        }

        function finalizeStream(): AssistantMessageEvent {
          const totalMs = Date.now() - t0;
          const ttftMs = firstTokenTime > 0 ? firstTokenTime - fetchStart : 0;
          const fetchMs = t3 - t2;
          callbacks.onDebug?.(`inference timing: total=${totalMs}ms, ttft=${ttftMs}ms, buildReq=${t1-t0}ms, onPayload=${t2-t1}ms, fetch=${fetchMs}ms, onResponse=${t4-t3}ms, bodyCheck=${t5-t4}ms, sse=${totalMs - (t5-t0)}ms`);
          if (!started) {
            partial.content.push({ type: "text", text: "" });
            textContentIndex = 0;
          }
          // Synchronize tool-call argument blocks with the accumulated buffer so
          // the finalMessage carries the complete parsed arguments object.
          for (const [tcIndex, ci] of toolCallContentIndices) {
            const buf = toolCallBuffers.get(tcIndex);
            if (!buf) continue;
            const block = partial.content[ci] as
              | { type: string; id: string; name: string; arguments: Record<string, unknown> }
              | undefined;
            if (!block) continue;
            block.id = block.id || buf.id;
            block.name = block.name || buf.name;
            block.arguments = parsePartialJson(buf.args);
          }
          // Emit block-end events before the done event so consumers receive the
          // full text/thinking/toolcall payload (especially the assembled tool
          // call arguments) when the stream closes.
          emitBlockEnds(partial);
          const finalMessage: AssistantMessage = {
            ...partial,
            content: partial.content.length > 0
              ? partial.content
              : [{ type: "text", text: fullText }],
            responseId,
            responseModel,
            usage,
            stopReason,
            timestamp: Date.now(),
          };
          if (kvTransferParams) {
            (finalMessage as unknown as Record<string, unknown>).kvTransferParams = kvTransferParams;
          }
          turnTiming = { totalMs, ttftMs };
          streamFinished = true;
          clearTimeout(timeout);
          const doneEvent: AssistantMessageEvent = {
            type: "done",
            reason: stopReason,
            message: finalMessage,
          };
          resultResolve?.(finalMessage);
          return doneEvent;
        }

        return {
          async next(): Promise<IteratorResult<AssistantMessageEvent>> {
            try {
              while (eventQueue.length === 0 && !streamFinished) {
                const result = await iterator.next();
                if (result.done) {
                  callbacks.onDebug?.("SSE iterator done, calling finalizeStream");
                  eventQueue.push(finalizeStream());
                  break;
                }
                const chunk = result.value;
                callbacks.onChunk?.(chunk);
                processChunk(chunk);
              }

              if (eventQueue.length > 0) {
                const event = eventQueue.shift()!;
                if (event.type === "done") {
                  callbacks.onDebug?.("dispatching done event, scheduling onDone");
                  // Fire-and-forget the onDone callback so the consumer is not
                  // blocked waiting for the turn_end HTTP call (up to 6s) to
                  // complete before receiving the done event. The previous
                  // implementation awaited onDone inside next(), which delayed
                  // the consumer's done event by up to requestTimeoutMs.
                  void Promise.resolve(
                    callbacks.onDone?.(event.message, {
                      messages: context.messages as unknown[],
                      timing: turnTiming,
                    }),
                  ).catch((err) => {
                    callbacks.onDebug?.(
                      `background onDone failed: ${err instanceof Error ? err.message : String(err)}`,
                    );
                  });
                }
                return { done: false, value: event };
              }

              callbacks.onDebug?.("queue empty and streamFinished, returning done:true");
              return { done: true, value: undefined };
            } catch (err) {
              callbacks.onDebug?.(`next() error: ${err instanceof Error ? err.message : String(err)}`);
              clearTimeout(timeout);
              const error = err instanceof Error ? err : new Error(String(err));
              callbacks.onError?.(error);
              if (!streamFinished) {
                const errorAssistantMessage: AssistantMessage = {
                  ...partial,
                  stopReason: "error",
                  timestamp: Date.now(),
                };
                streamFinished = true;
                resultResolve?.(errorAssistantMessage);
                return { done: false, value: { type: "error", reason: "error", error: errorAssistantMessage } };
              }
              return { done: true, value: undefined };
            }
          },
          async return(): Promise<IteratorResult<AssistantMessageEvent>> {
            clearTimeout(timeout);
            if (typeof iterator.return === "function") {
              await iterator.return(undefined as never);
            }
            return { done: true, value: undefined };
          },
          async throw(error?: unknown): Promise<IteratorResult<AssistantMessageEvent>> {
            clearTimeout(timeout);
            if (typeof iterator.throw === "function") {
              await iterator.throw(error);
            }
            throw error;
          },
        };
      },
      async result(): Promise<AssistantMessage> {
        return resultPromise;
      },
    };

    return self;
  };
}
