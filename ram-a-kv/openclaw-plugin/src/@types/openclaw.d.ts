declare module "openclaw/plugin-sdk/plugin-entry" {
  export interface DefinedPluginEntry {
    id: string;
    name: string;
    description: string;
    configSchema: unknown;
    register: (api: OpenClawPluginApi) => void;
  }

  export interface OpenClawPluginApi {
    pluginConfig: Record<string, unknown> | undefined;
    config: unknown;
    logger: PluginLogger;
    runtime: PluginRuntime;
    on<K extends PluginHookName>(
      hookName: K,
      handler: PluginHookHandlerMap[K],
      opts?: { priority?: number; timeoutMs?: number },
    ): void;
    registerProvider: (provider: ProviderPlugin) => void;
    registerService: (service: unknown) => void;
    registerHttpRoute: (route: unknown) => void;
    registerGatewayMethod: (
      method: string,
      handler: (params: unknown) => Promise<unknown>,
      opts?: { scope?: string },
    ) => void;
  }

  export interface PluginLogger {
    info(message: string): void;
    warn(message: string): void;
    error(message: string): void;
  }

  export interface PluginRuntime {
    config?: {
      current?: () => unknown;
    };
    state: {
      openKeyedStore<T>(opts: {
        namespace: string;
        maxEntries: number;
        overflowPolicy?: string;
      }): PluginStateKeyedStore<T>;
    };
  }

  export interface PluginStateKeyedStore<T> {
    register(key: string, value: T, opts?: { ttlMs?: number }): Promise<void>;
    registerIfAbsent(key: string, value: T, opts?: { ttlMs?: number }): Promise<boolean>;
    lookup(key: string): Promise<T | undefined>;
    consume(key: string): Promise<T | undefined>;
    delete(key: string): Promise<boolean>;
    entries(): Promise<Array<{ key: string; value: T; createdAt: number; expiresAt?: number }>>;
    clear(): Promise<void>;
  }

  export interface ProviderPlugin {
    id: string;
    label?: string;
    pluginId?: string;
    auth?: unknown[];
    createStreamFn?: (ctx: ProviderCreateStreamFnContext) => StreamFn | null | undefined;
    wrapStreamFn?: (ctx: ProviderWrapStreamFnContext) => StreamFn | null | undefined;
  }

  export interface ProviderCreateStreamFnContext {
    config?: unknown;
    agentDir?: string;
    workspaceDir?: string;
    agentId?: string;
    provider: string;
    modelId: string;
    model?: unknown;
    extraParams?: Record<string, unknown>;
  }

  export interface ProviderWrapStreamFnContext extends ProviderCreateStreamFnContext {
    streamFn?: StreamFn;
  }

  export type StreamFn = (
    model: unknown,
    context: unknown,
    options?: unknown,
  ) => unknown | Promise<unknown>;

  type PluginHookName =
    | "session_start"
    | "session_end"
    | "before_agent_run"
    | "agent_turn_prepare"
    | "model_call_ended"
    | "gateway_start"
    | "gateway_stop"
    | string;

  interface PluginHookSessionStartEvent {
    sessionId: string;
    sessionKey?: string;
    resumedFrom?: string;
  }

  interface PluginHookSessionEndEvent {
    sessionId: string;
    sessionKey?: string;
    messageCount: number;
    durationMs?: number;
    reason?: string;
    sessionFile?: string;
    transcriptArchived?: boolean;
    nextSessionId?: string;
    nextSessionKey?: string;
  }

  interface PluginHookBeforeAgentRunEvent {
    sessionId?: string;
    sessionKey?: string;
  }

  interface PluginHookBeforeAgentRunContext {
    sessionId?: string;
    sessionKey?: string;
  }

  interface PluginHookHandlerMap {
    session_start: (
      event: PluginHookSessionStartEvent,
      ctx: unknown,
    ) => Promise<void> | void;
    session_end: (
      event: PluginHookSessionEndEvent,
      ctx: unknown,
    ) => Promise<void> | void;
    before_agent_run: (
      event: PluginHookBeforeAgentRunEvent,
      ctx: PluginHookBeforeAgentRunContext,
    ) => Promise<void> | void;
    gateway_start: () => Promise<void> | void;
    gateway_stop: () => Promise<void> | void;
    [key: string]: ((...args: unknown[]) => unknown) | undefined;
  }

  export function definePluginEntry(opts: {
    id: string;
    name: string;
    description: string;
    configSchema?: unknown;
    register: (api: OpenClawPluginApi) => void;
  }): DefinedPluginEntry;
}
