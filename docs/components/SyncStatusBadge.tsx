"use client";

import { useEffect, useState } from "react";
import {
  CapTableSyncStatus,
  replayQueuedWrites,
  type QueuedWrite,
  type SyncStatus,
} from "@/lib/offline/cap_table_db";

const LABELS: Record<SyncStatus, string> = {
  synced: "Synced",
  syncing: "Syncing",
  offline: "Offline",
};

const STYLES: Record<SyncStatus, string> = {
  synced: "border-brand-500/25 bg-brand-500/[0.06] text-brand-300",
  syncing: "border-sky-500/25 bg-sky-500/[0.06] text-sky-300",
  offline: "border-gold-500/25 bg-gold-500/[0.06] text-gold-300",
};

/**
 * Visual `Synced` / `Syncing` / `Offline` indicator for the offline-first
 * cap-table cache (issue #294). Replays any writes queued while offline
 * automatically on reconnect, via [`CapTableSyncStatus`].
 */
export function SyncStatusBadge({
  sendWrite,
}: {
  /** Sends one queued write to the API; rejecting leaves it queued for the next reconnect. */
  sendWrite: (write: QueuedWrite) => Promise<void>;
}) {
  const [status, setStatus] = useState<SyncStatus>("synced");

  useEffect(() => {
    const tracker = new CapTableSyncStatus(async () => {
      await replayQueuedWrites(sendWrite);
    });
    const unsubscribe = tracker.subscribe(setStatus);
    return () => {
      unsubscribe();
      tracker.dispose();
    };
  }, [sendWrite]);

  return (
    <span
      role="status"
      className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium ${STYLES[status]}`}
    >
      <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full bg-current" />
      {LABELS[status]}
    </span>
  );
}
