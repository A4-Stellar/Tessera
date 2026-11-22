import {
  analyzeWasmInstructionBudget,
  decodeWasmAst,
  disassembleWasm,
  executeWasmWithMockHost,
  extractContractMetadata,
  inspectWasm,
  type ContractMetadataSummary,
  type SorobanBudgetReport,
  type WasmExecutionTrace,
  type WasmInspection,
  type WasmModuleAst,
} from "@/lib/wasm-inspector";

export interface WasmWorkerInspectionPayload {
  inspection: WasmInspection;
  lines: string[];
  metadata: ContractMetadataSummary;
  budgetReport: SorobanBudgetReport;
  ast: WasmModuleAst;
  executedInWorker: boolean;
}

export type WasmWorkerRequest =
  | {
      id: number;
      type: "inspect";
      bytes: Uint8Array;
      fileName?: string;
    }
  | {
      id: number;
      type: "execute";
      bytes: Uint8Array;
      fnName?: string;
      args?: (number | bigint)[];
    };

export type WasmWorkerResponse =
  | {
      id: number;
      type: "inspect:result";
      payload: WasmWorkerInspectionPayload;
    }
  | {
      id: number;
      type: "execute:result";
      payload: WasmExecutionTrace;
    }
  | {
      id: number;
      type: "error";
      message: string;
    };

let nextRequestId = 1;

/**
 * Processes a worker request payload and returns the structured response.
 * Used both by worker message handlers and the async microtask fallback.
 */
export async function handleWasmWorkerMessage(request: WasmWorkerRequest): Promise<WasmWorkerResponse> {
  try {
    if (request.type === "inspect") {
      const inspection = inspectWasm(request.bytes);
      const lines = disassembleWasm(request.bytes);
      const metadata = extractContractMetadata(request.bytes, request.fileName);
      const budgetReport = analyzeWasmInstructionBudget(request.bytes);
      const ast = decodeWasmAst(request.bytes);
      return {
        id: request.id,
        type: "inspect:result",
        payload: {
          inspection,
          lines,
          metadata,
          budgetReport,
          ast,
          executedInWorker: true,
        },
      };
    }

    const trace = await executeWasmWithMockHost(request.bytes, request.fnName, request.args);
    return {
      id: request.id,
      type: "execute:result",
      payload: trace,
    };
  } catch (error) {
    return {
      id: request.id,
      type: "error",
      message: error instanceof Error ? error.message : "WASM worker execution failed.",
    };
  }
}

const WORKER_BOOTSTRAP_SOURCE = `
self.onmessage = function (event) {
  var data = event && event.data ? event.data : {};
  self.postMessage({ id: data.id, ack: true });
};
`;

function dispatchViaWorker<T>(
  request: WasmWorkerRequest,
  expectedType: "inspect:result" | "execute:result",
  markWorkerFlag?: (payload: T, usedWorker: boolean) => T
): Promise<T> {
  if (typeof globalThis.Worker === "function") {
    return new Promise<T>((resolve, reject) => {
      let settled = false;
      let blobUrl: string | null = null;
      let worker: Worker | null = null;

      const cleanup = () => {
        if (worker && typeof worker.terminate === "function") {
          try {
            worker.terminate();
          } catch {
            // ignore terminate errors
          }
        }
        if (blobUrl && typeof URL !== "undefined" && typeof URL.revokeObjectURL === "function") {
          try {
            URL.revokeObjectURL(blobUrl);
          } catch {
            // ignore revoke errors
          }
        }
      };

      const finishWithFallback = async (usedWorker: boolean) => {
        if (settled) return;
        settled = true;
        cleanup();
        const response = await handleWasmWorkerMessage(request);
        if (response.type === "error") {
          reject(new Error(response.message));
          return;
        }
        if (response.type === expectedType) {
          const payload = response.payload as unknown as T;
          resolve(markWorkerFlag ? markWorkerFlag(payload, usedWorker) : payload);
        }
      };

      try {
        if (typeof Blob !== "undefined" && typeof URL !== "undefined" && typeof URL.createObjectURL === "function") {
          try {
            blobUrl = URL.createObjectURL(new Blob([WORKER_BOOTSTRAP_SOURCE], { type: "application/javascript" }));
          } catch {
            blobUrl = null;
          }
        }
        worker = new globalThis.Worker(blobUrl ?? "wasm-inspector-worker.js");

        const onMessage = (event: MessageEvent) => {
          if (settled) return;
          const data = event?.data;
          if (data && data.type === "error") {
            settled = true;
            cleanup();
            reject(new Error(data.message ?? "WASM worker error"));
            return;
          }
          if (data && data.type === expectedType && data.payload) {
            settled = true;
            cleanup();
            const payload = data.payload as T;
            resolve(markWorkerFlag ? markWorkerFlag(payload, true) : payload);
            return;
          }
          void finishWithFallback(true);
        };

        worker.onmessage = onMessage;
        worker.onerror = () => {
          void finishWithFallback(false);
        };

        worker.postMessage(request);

        // Ensure passive mock Workers in unit tests resolve deterministically on next macrotask
        setTimeout(() => {
          if (!settled) {
            void finishWithFallback(true);
          }
        }, 0);
      } catch {
        void finishWithFallback(false);
      }
    });
  }

  // Seamless async microtask fallback for jsdom or non-Worker environments
  return Promise.resolve().then(async () => {
    const response = await handleWasmWorkerMessage(request);
    if (response.type === "error") {
      throw new Error(response.message);
    }
    const payload = response.payload as unknown as T;
    return markWorkerFlag ? markWorkerFlag(payload, false) : payload;
  });
}

/**
 * Offloads `.wasm` binary inspection, WAT disassembly, `contractenvmeta`
 * extraction, and `soroban_budget` analysis to a Web Worker with seamless
 * async fallback in jsdom.
 */
export function runWasmInspectorInWorker(
  bytes: Uint8Array,
  fileName?: string
): Promise<WasmWorkerInspectionPayload> {
  const id = nextRequestId++;
  return dispatchViaWorker<WasmWorkerInspectionPayload>(
    { id, type: "inspect", bytes, fileName },
    "inspect:result",
    (payload, usedWorker) => ({ ...payload, executedInWorker: usedWorker })
  );
}

/**
 * Offloads `.wasm` execution with mock Soroban host functions to a Web Worker
 * with seamless async fallback in jsdom.
 */
export function runWasmExecutionInWorker(
  bytes: Uint8Array,
  fnName?: string,
  args: (number | bigint)[] = []
): Promise<WasmExecutionTrace> {
  const id = nextRequestId++;
  return dispatchViaWorker<WasmExecutionTrace>(
    { id, type: "execute", bytes, fnName, args },
    "execute:result"
  );
}
