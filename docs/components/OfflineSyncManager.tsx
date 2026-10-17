// Issue #204: Interactive Offline-First Cap-Table Offline Sync Component
// IndexedDB storage with Service Worker sync and conflict resolution

import React, { useState, useEffect, useCallback } from 'react';
import { openDB, DBSchema, IDBPDatabase } from 'idb';

interface CapTableDraft {
  id: string;
  shareholderId: string;
  shares: number;
  timestamp: number;
  synced: boolean;
  serverId?: string;
  version: number;
}

interface SyncConflict {
  localDraft: CapTableDraft;
  serverData: CapTableDraft;
}

interface CapTableDB extends DBSchema {
  drafts: {
    key: string;
    value: CapTableDraft;
    indexes: { 'by-synced': boolean; 'by-timestamp': number };
  };
  conflicts: {
    key: string;
    value: SyncConflict;
  };
}

interface OfflineSyncManagerProps {
  apiEndpoint: string;
  onSyncComplete?: (count: number) => void;
  onConflictDetected?: (conflicts: SyncConflict[]) => void;
}

export const OfflineSyncManager: React.FC<OfflineSyncManagerProps> = ({
  apiEndpoint,
  onSyncComplete,
  onConflictDetected,
}) => {
  const [isOnline, setIsOnline] = useState(navigator.onLine);
  const [pendingCount, setPendingCount] = useState(0);
  const [conflicts, setConflicts] = useState<SyncConflict[]>([]);
  const [db, setDb] = useState<IDBPDatabase<CapTableDB> | null>(null);
  const [showConflictModal, setShowConflictModal] = useState(false);
  const [syncing, setSyncing] = useState(false);

  // Initialize IndexedDB
  useEffect(() => {
    const initDB = async () => {
      const database = await openDB<CapTableDB>('cap-table-db', 1, {
        upgrade(db) {
          // Create drafts store
          const draftsStore = db.createObjectStore('drafts', { keyPath: 'id' });
          draftsStore.createIndex('by-synced', 'synced');
          draftsStore.createIndex('by-timestamp', 'timestamp');

          // Create conflicts store
          db.createObjectStore('conflicts', { keyPath: 'localDraft.id' });
        },
      });
      setDb(database);
      await updatePendingCount(database);
    };

    initDB();
  }, []);

  // Update pending count
  const updatePendingCount = async (database: IDBPDatabase<CapTableDB>) => {
    const count = await database.countFromIndex('drafts', 'by-synced', false);
    setPendingCount(count);
  };

  // Listen to online/offline events
  useEffect(() => {
    const handleOnline = () => {
      setIsOnline(true);
      syncDrafts();
    };

    const handleOffline = () => {
      setIsOnline(false);
    };

    window.addEventListener('online', handleOnline);
    window.addEventListener('offline', handleOffline);

    // Register service worker for background sync
    if ('serviceWorker' in navigator && 'SyncManager' in window) {
      navigator.serviceWorker.ready.then((registration) => {
        registration.sync.register('cap-table-sync');
      });
    }

    return () => {
      window.removeEventListener('online', handleOnline);
      window.removeEventListener('offline', handleOffline);
    };
  }, [db]);

  // Save draft to IndexedDB
  const saveDraft = useCallback(
    async (draft: Omit<CapTableDraft, 'id' | 'timestamp' | 'synced' | 'version'>) => {
      if (!db) return;

      const newDraft: CapTableDraft = {
        ...draft,
        id: `draft_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
        timestamp: Date.now(),
        synced: false,
        version: 1,
      };

      await db.add('drafts', newDraft);
      await updatePendingCount(db);

      // Trigger sync if online
      if (isOnline) {
        syncDrafts();
      }
    },
    [db, isOnline],
  );

  // Sync drafts to server
  const syncDrafts = useCallback(async () => {
    if (!db || !isOnline || syncing) return;

    setSyncing(true);

    try {
      // Get all unsynced drafts
      const unsyncedDrafts = await db.getAllFromIndex('drafts', 'by-synced', false);

      let syncedCount = 0;
      const detectedConflicts: SyncConflict[] = [];

      for (const draft of unsyncedDrafts) {
        try {
          // Send to server
          const response = await fetch(`${apiEndpoint}/cap-table/sync`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(draft),
          });

          if (response.ok) {
            const serverData = await response.json();

            // Check for conflicts (server version changed)
            if (draft.serverId && serverData.version !== draft.version) {
              detectedConflicts.push({
                localDraft: draft,
                serverData,
              });
              await db.add('conflicts', {
                localDraft: draft,
                serverData,
              });
            } else {
              // Mark as synced
              draft.synced = true;
              draft.serverId = serverData.id;
              draft.version = serverData.version;
              await db.put('drafts', draft);
              syncedCount++;
            }
          }
        } catch (error) {
          console.error('Sync error for draft:', draft.id, error);
        }
      }

      await updatePendingCount(db);

      if (detectedConflicts.length > 0) {
        setConflicts(detectedConflicts);
        setShowConflictModal(true);
        onConflictDetected?.(detectedConflicts);
      }

      if (syncedCount > 0) {
        onSyncComplete?.(syncedCount);
      }
    } catch (error) {
      console.error('Sync failed:', error);
    } finally {
      setSyncing(false);
    }
  }, [db, isOnline, syncing, apiEndpoint, onSyncComplete, onConflictDetected]);

  // Resolve conflict
  const resolveConflict = useCallback(
    async (conflictId: string, resolution: 'local' | 'server') => {
      if (!db) return;

      const conflict = await db.get('conflicts', conflictId);
      if (!conflict) return;

      if (resolution === 'local') {
        // Keep local version, force update server
        const response = await fetch(`${apiEndpoint}/cap-table/force-update`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(conflict.localDraft),
        });

        if (response.ok) {
          const updated = conflict.localDraft;
          updated.synced = true;
          await db.put('drafts', updated);
        }
      } else {
        // Accept server version, discard local
        await db.delete('drafts', conflict.localDraft.id);
      }

      // Remove conflict
      await db.delete('conflicts', conflictId);

      // Update conflicts list
      const remainingConflicts = await db.getAll('conflicts');
      setConflicts(remainingConflicts);

      if (remainingConflicts.length === 0) {
        setShowConflictModal(false);
      }

      await updatePendingCount(db);
    },
    [db, apiEndpoint],
  );

  return (
    <div className="offline-sync-manager p-4 bg-white rounded-lg shadow">
      {/* Status Indicator */}
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-2">
          <div
            className={`w-3 h-3 rounded-full ${
              isOnline ? 'bg-green-500' : 'bg-red-500'
            }`}
          />
          <span className="text-sm font-medium">
            {isOnline ? 'Online' : 'Offline'}
          </span>
        </div>

        {pendingCount > 0 && (
          <div className="flex items-center gap-2">
            <span className="text-sm text-gray-600">
              {pendingCount} pending change{pendingCount > 1 ? 's' : ''}
            </span>
            {isOnline && (
              <button
                onClick={syncDrafts}
                disabled={syncing}
                className="px-3 py-1 text-sm bg-blue-500 text-white rounded hover:bg-blue-600 disabled:opacity-50"
              >
                {syncing ? 'Syncing...' : 'Sync Now'}
              </button>
            )}
          </div>
        )}
      </div>

      {/* Conflict Resolution Modal */}
      {showConflictModal && conflicts.length > 0 && (
        <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
          <div className="bg-white rounded-lg p-6 max-w-2xl w-full max-h-[80vh] overflow-y-auto">
            <h2 className="text-xl font-bold mb-4">Sync Conflicts Detected</h2>
            <p className="text-gray-600 mb-4">
              The server has newer versions of these cap table entries. Choose how to
              resolve each conflict:
            </p>

            {conflicts.map((conflict) => (
              <div
                key={conflict.localDraft.id}
                className="border rounded p-4 mb-4 bg-gray-50"
              >
                <h3 className="font-semibold mb-2">
                  Shareholder: {conflict.localDraft.shareholderId}
                </h3>

                <div className="grid grid-cols-2 gap-4 mb-4">
                  <div className="border-r pr-4">
                    <h4 className="text-sm font-medium text-gray-700 mb-2">
                      Your Changes
                    </h4>
                    <p className="text-sm">
                      Shares: {conflict.localDraft.shares}
                    </p>
                    <p className="text-xs text-gray-500">
                      Modified:{' '}
                      {new Date(conflict.localDraft.timestamp).toLocaleString()}
                    </p>
                  </div>

                  <div className="pl-4">
                    <h4 className="text-sm font-medium text-gray-700 mb-2">
                      Server Version
                    </h4>
                    <p className="text-sm">Shares: {conflict.serverData.shares}</p>
                    <p className="text-xs text-gray-500">
                      Modified:{' '}
                      {new Date(conflict.serverData.timestamp).toLocaleString()}
                    </p>
                  </div>
                </div>

                <div className="flex gap-2">
                  <button
                    onClick={() =>
                      resolveConflict(conflict.localDraft.id, 'local')
                    }
                    className="flex-1 px-4 py-2 bg-blue-500 text-white rounded hover:bg-blue-600"
                  >
                    Keep My Changes
                  </button>
                  <button
                    onClick={() =>
                      resolveConflict(conflict.localDraft.id, 'server')
                    }
                    className="flex-1 px-4 py-2 bg-gray-500 text-white rounded hover:bg-gray-600"
                  >
                    Use Server Version
                  </button>
                </div>
              </div>
            ))}

            <button
              onClick={() => setShowConflictModal(false)}
              className="w-full mt-4 px-4 py-2 bg-gray-200 text-gray-800 rounded hover:bg-gray-300"
            >
              Close
            </button>
          </div>
        </div>
      )}

      {/* Offline Notice */}
      {!isOnline && (
        <div className="mt-4 p-3 bg-yellow-50 border border-yellow-200 rounded">
          <p className="text-sm text-yellow-800">
            You're offline. Changes will be saved locally and synced when connection
            is restored.
          </p>
        </div>
      )}
    </div>
  );
};

export default OfflineSyncManager;
