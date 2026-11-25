// @vitest-environment node
import { describe, it, expect, vi, afterEach } from "vitest";
import {
  BENCHMARK_BUDGET_MS,
  BENCHMARK_ITEM_COUNT,
  WEBCRYPTO_MIN_ITEM_BYTES,
  hashBatch,
  hashBatchSync,
  resolveBackend,
  type HashAlgorithm,
} from "../lib/hashing";
import { hashInWorker } from "../lib/hashWorkerClient";

const enc = new TextEncoder();
const ALGORITHMS: HashAlgorithm[] = ["sha256", "keccak256"];

const VECTORS: Record<HashAlgorithm, Record<string, string>> = {
  sha256: {
    "": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    abc: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
  },
  keccak256: {
    "": "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470",
    abc: "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45",
  },
};

function makeItems(count: number, size = 48): Uint8Array[] {
  return Array.from({ length: count }, (_, i) => {
    const bytes = new Uint8Array(size);
    bytes.set(enc.encode(`item-${i}`));
    return bytes;
  });
}

/** Mock Worker that hashes with the real batch function, like the real worker would. */
class MockHashWorker {
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: ((e: ErrorEvent) => void) | null = null;
  terminated = false;
  postMessage(req: { id: number; algorithm: HashAlgorithm; items: Uint8Array[] }) {
    void hashBatch(req.items, req.algorithm).then((digests) =>
      this.onmessage?.({ data: { id: req.id, digests } } as MessageEvent),
    );
  }
  terminate() {
    this.terminated = true;
  }
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("hashing correctness", () => {
  for (const algorithm of ALGORITHMS) {
    it(`${algorithm} matches known test vectors`, () => {
      for (const [input, expected] of Object.entries(VECTORS[algorithm])) {
        expect(hashBatchSync([enc.encode(input)], algorithm)[0]).toBe(expected);
      }
    });
  }
});

describe("10,000 item benchmark (Issue #505)", () => {
  for (const algorithm of ALGORITHMS) {
    it(`${algorithm}: hashes ${BENCHMARK_ITEM_COUNT} items in under ${BENCHMARK_BUDGET_MS}ms`, async () => {
      const items = makeItems(BENCHMARK_ITEM_COUNT);
      await hashBatch(items.slice(0, 200), algorithm); // warm-up, excluded from timing
      const start = performance.now();
      const digests = await hashBatch(items, algorithm);
      const duration = performance.now() - start;
      expect(digests).toHaveLength(BENCHMARK_ITEM_COUNT);
      expect(new Set(digests).size).toBe(BENCHMARK_ITEM_COUNT);
      expect(duration).toBeLessThan(BENCHMARK_BUDGET_MS);
    });
  }

  it("worker client returns the same digests as direct hashing", async () => {
    const items = makeItems(BENCHMARK_ITEM_COUNT);
    const worker = new MockHashWorker();
    const result = await hashInWorker(items, "keccak256", {
      workerFactory: () => worker as unknown as Worker,
    });
    expect(result.mode).toBe("worker");
    expect(result.digests).toEqual(hashBatchSync(items, "keccak256"));
    expect(worker.terminated).toBe(true);
  });
});

describe("main thread stays unblocked", () => {
  it("the chunked fallback yields so a 4ms ticker never stalls for a long frame", async () => {
    const items = makeItems(BENCHMARK_ITEM_COUNT);
    let last = performance.now();
    let maxGap = 0;
    const ticker = setInterval(() => {
      const now = performance.now();
      maxGap = Math.max(maxGap, now - last);
      last = now;
    }, 4);

    const result = await hashInWorker(items, "keccak256", {
      forceMainThread: true,
      chunkSize: 250,
    });
    clearInterval(ticker);

    expect(result.mode).toBe("main-thread");
    expect(result.digests).toHaveLength(BENCHMARK_ITEM_COUNT);
    // Three 60 FPS frames. A single blocking 10,000-item run would exceed this on slow CI.
    expect(maxGap).toBeLessThan(50);
  });
});

describe("fallback without native crypto acceleration", () => {
  const large = () => makeItems(4, WEBCRYPTO_MIN_ITEM_BYTES);

  it("auto picks Web Crypto only for large SHA-256 items and never for Keccak", () => {
    expect(resolveBackend("sha256", makeItems(10))).toBe("js");
    expect(resolveBackend("sha256", large())).toBe("webcrypto");
    expect(resolveBackend("keccak256", large(), "webcrypto")).toBe("js");
    expect(resolveBackend("sha256", large(), "js")).toBe("js");
  });

  it("falls back to pure JS when crypto.subtle is missing", async () => {
    const items = large();
    const expected = await hashBatch(items, "sha256", "js");
    vi.stubGlobal("crypto", {});
    expect(resolveBackend("sha256", items, "webcrypto")).toBe("js");
    expect(await hashBatch(items, "sha256", "webcrypto")).toEqual(expected);
  });

  it("falls back to pure JS when WebAssembly and crypto.subtle are both missing", async () => {
    vi.stubGlobal("crypto", undefined);
    vi.stubGlobal("WebAssembly", undefined);
    const items = makeItems(BENCHMARK_ITEM_COUNT);
    for (const algorithm of ALGORITHMS) {
      const start = performance.now();
      const digests = await hashBatch(items, algorithm);
      expect(performance.now() - start).toBeLessThan(BENCHMARK_BUDGET_MS);
      expect(digests[0]).toBe(hashBatchSync([items[0]], algorithm)[0]);
    }
  });

  it("falls back to pure JS when native digest throws", async () => {
    const items = large();
    const expected = hashBatchSync(items, "sha256");
    vi.stubGlobal("crypto", {
      subtle: { digest: () => Promise.reject(new Error("insecure context")) },
    });
    expect(await hashBatch(items, "sha256", "webcrypto")).toEqual(expected);
  });

  it("Web Crypto and JS backends produce identical SHA-256 digests", async () => {
    const items = large();
    expect(await hashBatch(items, "sha256", "webcrypto")).toEqual(
      await hashBatch(items, "sha256", "js"),
    );
  });

  it("falls back to main-thread hashing when the worker cannot be created", async () => {
    const items = makeItems(1000);
    const result = await hashInWorker(items, "sha256", {
      workerFactory: () => {
        throw new Error("Worker blocked by CSP");
      },
    });
    expect(result.mode).toBe("main-thread");
    expect(result.digests).toEqual(hashBatchSync(items, "sha256"));
  });

  it("falls back to main-thread hashing when the worker reports an error", async () => {
    const items = makeItems(100);
    const crashing = {
      onmessage: null as ((e: MessageEvent) => void) | null,
      onerror: null as ((e: ErrorEvent) => void) | null,
      postMessage() {
        this.onerror?.({ message: "boom" } as ErrorEvent);
      },
      terminate: vi.fn(),
    };
    const result = await hashInWorker(items, "keccak256", {
      workerFactory: () => crashing as unknown as Worker,
    });
    expect(result.mode).toBe("main-thread");
    expect(crashing.terminate).toHaveBeenCalled();
    expect(result.digests).toEqual(hashBatchSync(items, "keccak256"));
  });
});
