/**
 * Runs batch hashing in a Web Worker, falling back to chunked main-thread hashing
 * when workers are unavailable or blocked (CSP, sandbox, old browser) (Issue #505).
 */
import { hashBatch, type HashAlgorithm, type HashBackend } from "./hashing";
import type { HashWorkerRequest, HashWorkerResponse } from "../workers/hash.worker";

export interface HashRunOptions {
  backend?: HashBackend;
  /** Override worker creation (used by tests). */
  workerFactory?: () => Worker;
  /** Skip the worker and hash on the main thread. */
  forceMainThread?: boolean;
  /** Items hashed between yields to the event loop in the fallback path. */
  chunkSize?: number;
}

export interface HashRunResult {
  digests: string[];
  durationMs: number;
  mode: "worker" | "main-thread";
}

export const DEFAULT_CHUNK_SIZE = 500;

function defaultWorkerFactory(): Worker {
  return new Worker(new URL("../workers/hash.worker.ts", import.meta.url), { type: "module" });
}

/** Gives the browser a chance to render and handle input between chunks. */
function yieldToMain(): Promise<void> {
  const scheduler = (globalThis as { scheduler?: { yield?: () => Promise<void> } }).scheduler;
  if (typeof scheduler?.yield === "function") return scheduler.yield();
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function runInWorker(
  worker: Worker,
  items: Uint8Array[],
  algorithm: HashAlgorithm,
  backend: HashBackend | undefined,
): Promise<string[]> {
  return new Promise((resolve, reject) => {
    const request: HashWorkerRequest = { id: 1, algorithm, backend, items };
    worker.onmessage = (event: MessageEvent<HashWorkerResponse>) => {
      const data = event.data;
      if ("error" in data) reject(new Error(data.error));
      else resolve(data.digests);
    };
    worker.onerror = (event) => reject(new Error(event.message || "Hash worker failed"));
    worker.postMessage(request);
  });
}

async function runChunked(
  items: Uint8Array[],
  algorithm: HashAlgorithm,
  backend: HashBackend | undefined,
  chunkSize: number,
): Promise<string[]> {
  const digests: string[] = [];
  for (let i = 0; i < items.length; i += chunkSize) {
    digests.push(...(await hashBatch(items.slice(i, i + chunkSize), algorithm, backend)));
    if (i + chunkSize < items.length) await yieldToMain();
  }
  return digests;
}

export async function hashInWorker(
  items: Uint8Array[],
  algorithm: HashAlgorithm,
  options: HashRunOptions = {},
): Promise<HashRunResult> {
  const start = performance.now();
  const { backend, forceMainThread, chunkSize = DEFAULT_CHUNK_SIZE } = options;

  if (!forceMainThread && (options.workerFactory || typeof Worker !== "undefined")) {
    let worker: Worker | undefined;
    try {
      worker = (options.workerFactory ?? defaultWorkerFactory)();
      const digests = await runInWorker(worker, items, algorithm, backend);
      return { digests, durationMs: performance.now() - start, mode: "worker" };
    } catch {
      // Worker blocked or crashed: fall through to the main-thread path.
    } finally {
      worker?.terminate();
    }
  }

  const digests = await runChunked(items, algorithm, backend, chunkSize);
  return { digests, durationMs: performance.now() - start, mode: "main-thread" };
}
