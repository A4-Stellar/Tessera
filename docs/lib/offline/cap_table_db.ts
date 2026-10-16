/**
 * Offline-first cap-table cache (issue #294).
 *
 * Caches `GET /assets/:id/holders` responses in IndexedDB so the cap-table
 * views in `CapTableVisualizer`/`AuditDashboard` keep showing the last known
 * state while offline, and queues any writes attempted while offline so they
 * replay once connectivity returns. Uses the browser's native `indexedDB`
 * directly — no `idb`/`dexie` dependency needed for a store this small.
 *
 * Not a general-purpose cache: one object store holds the latest holder
 * snapshot per asset, and one holds a FIFO queue of pending writes. There is
 * no eviction policy because a holder snapshot is small and bounded by the
 * number of assets a client actually views.
 */

export interface OnChainHolder {
  address: string;
  balance: string;
  share_percent?: number;
}

export type SyncStatus = "synced" | "syncing" | "offline";

export interface CachedSnapshot {
  assetId: string;
  holders: OnChainHolder[];
  /** Epoch ms when this snapshot was last confirmed fresh from the network. */
  fetchedAt: number;
}

/** A write queued while offline, to be replayed in the order it was made. */
export interface QueuedWrite {
  id: string;
  assetId: string;
  method: "POST" | "PUT" | "PATCH" | "DELETE";
  path: string;
  body: unknown;
  queuedAt: number;
}

const DB_NAME = "tessera-cap-table-offline";
const DB_VERSION = 1;
const SNAPSHOTS_STORE = "snapshots";
const QUEUE_STORE = "write_queue";

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(SNAPSHOTS_STORE)) {
        db.createObjectStore(SNAPSHOTS_STORE, { keyPath: "assetId" });
      }
      if (!db.objectStoreNames.contains(QUEUE_STORE)) {
        db.createObjectStore(QUEUE_STORE, { keyPath: "id" });
      }
    };

    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function runTransaction<T>(
  db: IDBDatabase,
  storeName: string,
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(storeName, mode);
    const store = tx.objectStore(storeName);
    const request = run(store);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

/**
 * Persists the latest holder snapshot for an asset, overwriting whatever was
 * cached before. Call this whenever a live fetch of `/assets/:id/holders`
 * succeeds.
 */
export async function cacheSnapshot(
  assetId: string,
  holders: OnChainHolder[],
): Promise<void> {
  const db = await openDb();
  const snapshot: CachedSnapshot = { assetId, holders, fetchedAt: Date.now() };
  await runTransaction(db, SNAPSHOTS_STORE, "readwrite", (store) =>
    store.put(snapshot),
  );
  db.close();
}

/** Reads back the last cached snapshot for an asset, or `null` if none exists yet. */
export async function getCachedSnapshot(
  assetId: string,
): Promise<CachedSnapshot | null> {
  const db = await openDb();
  const result = await runTransaction<CachedSnapshot | undefined>(
    db,
    SNAPSHOTS_STORE,
    "readonly",
    (store) => store.get(assetId),
  );
  db.close();
  return result ?? null;
}

/**
 * Queues a write made while offline. Returns the generated queue entry id so
 * a caller can show it as a pending/optimistic row.
 */
export async function queueWrite(
  write: Omit<QueuedWrite, "id" | "queuedAt">,
): Promise<string> {
  const db = await openDb();
  const id = `${write.assetId}_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
  const entry: QueuedWrite = { ...write, id, queuedAt: Date.now() };
  await runTransaction(db, QUEUE_STORE, "readwrite", (store) =>
    store.put(entry),
  );
  db.close();
  return id;
}

/** Every queued write still pending, oldest first. */
export async function listQueuedWrites(): Promise<QueuedWrite[]> {
  const db = await openDb();
  const all = await runTransaction<QueuedWrite[]>(
    db,
    QUEUE_STORE,
    "readonly",
    (store) => store.getAll(),
  );
  db.close();
  return all.sort((a, b) => a.queuedAt - b.queuedAt);
}

/** Removes one write from the queue once it has been successfully replayed. */
export async function removeQueuedWrite(id: string): Promise<void> {
  const db = await openDb();
  await runTransaction(db, QUEUE_STORE, "readwrite", (store) =>
    store.delete(id),
  );
  db.close();
}

/**
 * Replays every queued write in order against `sendWrite`, removing each one
 * from the queue as it succeeds. Stops at the first failure (network still
 * flaky, server rejected it) and leaves the rest queued, rather than
 * reordering writes around a stuck one.
 */
export async function replayQueuedWrites(
  sendWrite: (write: QueuedWrite) => Promise<void>,
): Promise<{ replayed: string[]; failedAt?: string }> {
  const pending = await listQueuedWrites();
  const replayed: string[] = [];

  for (const write of pending) {
    try {
      await sendWrite(write);
      await removeQueuedWrite(write.id);
      replayed.push(write.id);
    } catch {
      return { replayed, failedAt: write.id };
    }
  }

  return { replayed };
}

type SyncStatusListener = (status: SyncStatus) => void;

/**
 * Tracks online/offline transitions and a transient "syncing" state while a
 * replay is in flight, for a `Synced` / `Syncing` / `Offline` status widget.
 * `onReconnect` is invoked automatically on every `offline -> online`
 * transition so a caller just needs to pass `replayQueuedWrites` (or a
 * wrapper around it) once.
 */
export class CapTableSyncStatus {
  private status: SyncStatus;
  private readonly listeners = new Set<SyncStatusListener>();
  private readonly onOnline = () => void this.handleOnline();
  private readonly onOffline = () => this.setStatus("offline");

  constructor(private readonly onReconnect?: () => Promise<void>) {
    this.status =
      typeof navigator !== "undefined" && navigator.onLine === false
        ? "offline"
        : "synced";

    if (typeof window !== "undefined") {
      window.addEventListener("online", this.onOnline);
      window.addEventListener("offline", this.onOffline);
    }
  }

  get current(): SyncStatus {
    return this.status;
  }

  subscribe(listener: SyncStatusListener): () => void {
    this.listeners.add(listener);
    listener(this.status);
    return () => this.listeners.delete(listener);
  }

  /** Removes the `online`/`offline` window listeners. Call on component unmount. */
  dispose(): void {
    if (typeof window !== "undefined") {
      window.removeEventListener("online", this.onOnline);
      window.removeEventListener("offline", this.onOffline);
    }
    this.listeners.clear();
  }

  private setStatus(status: SyncStatus): void {
    this.status = status;
    for (const listener of this.listeners) listener(status);
  }

  private async handleOnline(): Promise<void> {
    this.setStatus("syncing");
    try {
      await this.onReconnect?.();
    } finally {
      this.setStatus("synced");
    }
  }
}
