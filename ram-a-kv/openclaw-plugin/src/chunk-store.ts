export interface ChunkStoreEntry {
  chunkHashes: string[];
  reason?: string;
  createdAt: number;
}

const STORE_NAMESPACE = "ram-a-kv-hook-chunks";
const STORE_MAX_ENTRIES = 10000;

export class ChunkStore {
  private store: {
    register(key: string, value: ChunkStoreEntry): Promise<void>;
    registerIfAbsent(
      key: string,
      value: ChunkStoreEntry,
    ): Promise<boolean>;
    lookup(key: string): Promise<ChunkStoreEntry | undefined>;
    consume(key: string): Promise<ChunkStoreEntry | undefined>;
    delete(key: string): Promise<boolean>;
  } | null = null;

  init(api: {
    runtime: {
      state: {
        openKeyedStore<T>(opts: {
          namespace: string;
          maxEntries: number;
          overflowPolicy?: string;
        }): {
          register(key: string, value: T): Promise<void>;
          registerIfAbsent(key: string, value: T): Promise<boolean>;
          lookup(key: string): Promise<T | undefined>;
          consume(key: string): Promise<T | undefined>;
          delete(key: string): Promise<boolean>;
        };
      };
    };
  }): void {
    this.store = api.runtime.state.openKeyedStore<ChunkStoreEntry>({
      namespace: STORE_NAMESPACE,
      maxEntries: STORE_MAX_ENTRIES,
      overflowPolicy: "evict-oldest",
    });
  }

  private requireStore() {
    if (!this.store) {
      throw new Error("ChunkStore not initialized");
    }
    return this.store;
  }

  async saveChunkHashes(
    sessionId: string,
    chunkHashes: string[],
    reason?: string,
  ): Promise<void> {
    if (!chunkHashes.length) return;
    await this.requireStore().register(sessionId, {
      chunkHashes,
      reason,
      createdAt: Date.now(),
    });
  }

  async saveReason(sessionId: string, reason: string): Promise<void> {
    const existing = await this.requireStore().lookup(sessionId);
    if (existing) {
      existing.reason = reason;
      await this.requireStore().register(sessionId, existing);
    } else {
      await this.requireStore().register(sessionId, {
        chunkHashes: [],
        reason,
        createdAt: Date.now(),
      });
    }
  }

  async getChunkHashes(sessionId: string): Promise<string[] | undefined> {
    const entry = await this.requireStore().lookup(sessionId);
    return entry?.chunkHashes.length ? entry.chunkHashes : undefined;
  }

  async getReason(sessionId: string): Promise<string | undefined> {
    const entry = await this.requireStore().lookup(sessionId);
    return entry?.reason;
  }

  async consumeReason(sessionId: string): Promise<string | undefined> {
    const entry = await this.requireStore().lookup(sessionId);
    if (!entry?.reason) return undefined;
    const reason = entry.reason;
    if (!entry.chunkHashes.length) {
      await this.requireStore().delete(sessionId);
    } else {
      await this.requireStore().register(sessionId, {
        ...entry,
        reason: undefined,
      });
    }
    return reason;
  }

  async remove(sessionId: string): Promise<void> {
    await this.requireStore().delete(sessionId);
  }
}
