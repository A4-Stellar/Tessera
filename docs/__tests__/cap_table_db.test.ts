import "fake-indexeddb/auto";

import {
  CapTableSyncStatus,
  cacheSnapshot,
  getCachedSnapshot,
  listQueuedWrites,
  queueWrite,
  removeQueuedWrite,
  replayQueuedWrites,
  type OnChainHolder,
} from "@/lib/offline/cap_table_db";

const HOLDERS: OnChainHolder[] = [
  { address: "GALICE", balance: "1000", share_percent: 60 },
  { address: "GBOB", balance: "667", share_percent: 40 },
];

describe("cap_table_db", () => {
  it("returns null for an asset that has never been cached", async () => {
    expect(await getCachedSnapshot("never-cached")).toBeNull();
  });

  it("caches and reads back a holder snapshot", async () => {
    await cacheSnapshot("asset-1", HOLDERS);
    const snapshot = await getCachedSnapshot("asset-1");

    expect(snapshot).not.toBeNull();
    expect(snapshot!.holders).toEqual(HOLDERS);
    expect(typeof snapshot!.fetchedAt).toBe("number");
  });

  it("overwrites the previous snapshot for the same asset", async () => {
    await cacheSnapshot("asset-2", HOLDERS);
    const updated: OnChainHolder[] = [{ address: "GCAROL", balance: "500" }];
    await cacheSnapshot("asset-2", updated);

    const snapshot = await getCachedSnapshot("asset-2");
    expect(snapshot!.holders).toEqual(updated);
  });

  it("queues a write and lists it back oldest first", async () => {
    const id1 = await queueWrite({
      assetId: "asset-3",
      method: "POST",
      path: "/assets/asset-3/transfers",
      body: { to: "GBOB", amount: "10" },
    });
    const id2 = await queueWrite({
      assetId: "asset-3",
      method: "POST",
      path: "/assets/asset-3/transfers",
      body: { to: "GCAROL", amount: "20" },
    });

    const pending = await listQueuedWrites();
    const ids = pending.map((w) => w.id);
    expect(ids.indexOf(id1)).toBeLessThan(ids.indexOf(id2));

    await removeQueuedWrite(id1);
    await removeQueuedWrite(id2);
  });

  it("replays queued writes in order and clears them on success", async () => {
    await queueWrite({
      assetId: "asset-4",
      method: "PATCH",
      path: "/assets/asset-4/holders/GALICE",
      body: { balance: "900" },
    });
    await queueWrite({
      assetId: "asset-4",
      method: "PATCH",
      path: "/assets/asset-4/holders/GBOB",
      body: { balance: "767" },
    });

    const sent: unknown[] = [];
    const result = await replayQueuedWrites(async (write) => {
      sent.push(write.body);
    });

    expect(result.replayed).toHaveLength(2);
    expect(result.failedAt).toBeUndefined();
    expect(sent).toEqual([{ balance: "900" }, { balance: "767" }]);
    expect(await listQueuedWrites()).toHaveLength(0);
  });

  it("stops replaying at the first failure and leaves the rest queued", async () => {
    await queueWrite({
      assetId: "asset-5",
      method: "POST",
      path: "/assets/asset-5/transfers",
      body: { step: 1 },
    });
    const secondId = await queueWrite({
      assetId: "asset-5",
      method: "POST",
      path: "/assets/asset-5/transfers",
      body: { step: 2 },
    });

    const result = await replayQueuedWrites(async (write) => {
      if (write.body && (write.body as { step: number }).step === 1) {
        throw new Error("network still down");
      }
    });

    expect(result.replayed).toHaveLength(0);
    expect(result.failedAt).toBeDefined();

    const stillQueued = await listQueuedWrites();
    expect(stillQueued.map((w) => w.id)).toContain(secondId);

    for (const write of stillQueued) await removeQueuedWrite(write.id);
  });
});

describe("CapTableSyncStatus", () => {
  function setOnline(value: boolean) {
    Object.defineProperty(window.navigator, "onLine", {
      configurable: true,
      value,
    });
  }

  afterEach(() => setOnline(true));

  it("starts synced when the browser reports online", () => {
    setOnline(true);
    const status = new CapTableSyncStatus();
    expect(status.current).toBe("synced");
    status.dispose();
  });

  it("starts offline when the browser reports offline", () => {
    setOnline(false);
    const status = new CapTableSyncStatus();
    expect(status.current).toBe("offline");
    status.dispose();
  });

  it("goes offline -> syncing -> synced across a reconnect, running onReconnect", async () => {
    setOnline(false);
    let reconnected = false;
    const status = new CapTableSyncStatus(async () => {
      reconnected = true;
    });
    const seen: string[] = [];
    status.subscribe((s) => seen.push(s));

    setOnline(true);
    window.dispatchEvent(new Event("online"));
    // Flush the microtask queue the async onReconnect runs in.
    await Promise.resolve();
    await Promise.resolve();

    expect(reconnected).toBe(true);
    expect(seen).toContain("syncing");
    expect(status.current).toBe("synced");
    status.dispose();
  });

  it("goes to offline on an offline event", () => {
    const status = new CapTableSyncStatus();
    window.dispatchEvent(new Event("offline"));
    expect(status.current).toBe("offline");
    status.dispose();
  });

  it("stops updating after dispose", () => {
    const status = new CapTableSyncStatus();
    status.dispose();
    window.dispatchEvent(new Event("offline"));
    expect(status.current).toBe("synced");
  });
});
