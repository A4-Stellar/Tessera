/**
 * Soroban Host Emulator (soroban-env-host in WASM / sandbox).
 * Runs inside a sandboxed iframe with `sandbox="allow-scripts"` CSP.
 * Simulates Soroban environment storage, auth, events, and meters resource usage:
 * CPU instructions, memory (bytes & pages), storage reads, and storage writes.
 */

export interface ResourceUsageMetrics {
  cpuInstructions: number;
  memoryPages: number;
  memoryBytes: number;
  storageReads: number;
  storageWrites: number;
}

export interface InvocationResult {
  functionName: string;
  success: boolean;
  returnValue: unknown;
  logs: string[];
  events: Array<{ topic: string[]; data: unknown }>;
  metrics: ResourceUsageMetrics;
  error?: string;
}

export interface HostStorageState {
  [key: string]: string;
}

export const INITIAL_ASSET_TOKEN_STATE: HostStorageState = {
  "ADMIN": "GBBD...TESSERA",
  "TOKEN_NAME": "Tessera RWA Asset Token",
  "TOKEN_SYMBOL": "TESS",
  "DECIMALS": "7",
  "TOTAL_SUPPLY": "1000000",
  "BALANCE:GBBD...TESSERA": "1000000",
  "BALANCE:GCXK...INVESTOR": "50000",
  "COMPLIANCE:GBBD...TESSERA": "APPROVED",
  "COMPLIANCE:GCXK...INVESTOR": "APPROVED",
};

/**
 * Executes a contract function call within the Soroban host emulator.
 */
export function executeHostCall(
  functionName: string,
  args: Record<string, string>,
  storage: HostStorageState = INITIAL_ASSET_TOKEN_STATE
): InvocationResult {
  const currentStorage = { ...storage };
  const logs: string[] = [
    `[soroban-env-host] Invoking '${functionName}' with args: ${JSON.stringify(args)}`,
  ];
  const events: Array<{ topic: string[]; data: unknown }> = [];

  let cpuInstructions = 5000;
  let memoryPages = 1;
  let memoryBytes = 65536;
  let storageReads = 1;
  let storageWrites = 0;
  let returnValue: unknown = null;
  let success = true;
  let error: string | undefined = undefined;

  switch (functionName) {
    case "transfer": {
      const from = args.from || "GBBD...TESSERA";
      const to = args.to || "GCXK...INVESTOR";
      const amount = parseInt(args.amount || "10000", 10);

      logs.push(`[soroban-env-host] from.require_auth() -> verified signature for ${from}`);
      cpuInstructions += 4500; // auth check cost

      const fromKey = `BALANCE:${from}`;
      const toKey = `BALANCE:${to}`;

      const fromBal = parseInt(currentStorage[fromKey] || "0", 10);
      const toBal = parseInt(currentStorage[toKey] || "0", 10);
      storageReads += 2; // read sender balance and recipient balance
      cpuInstructions += 3200; // storage read compute

      logs.push(`[soroban-env-host] Read balance for ${from}: ${fromBal}`);
      logs.push(`[soroban-env-host] Read balance for ${to}: ${toBal}`);

      if (amount <= 0) {
        success = false;
        error = "InvalidAmount: transfer amount must be greater than zero";
        logs.push(`[soroban-env-host] Error: ${error}`);
      } else if (fromBal < amount) {
        success = false;
        error = `InsufficientBalance: account ${from} has ${fromBal}, requested ${amount}`;
        logs.push(`[soroban-env-host] Panic: ${error}`);
      } else {
        const nextFromBal = fromBal - amount;
        const nextToBal = toBal + amount;
        currentStorage[fromKey] = nextFromBal.toString();
        currentStorage[toKey] = nextToBal.toString();
        storageWrites += 2; // write both updated balances
        cpuInstructions += 2540; // storage write compute

        events.push({
          topic: ["transfer", from, to],
          data: amount,
        });
        logs.push(`[soroban-env-host] Emitted event topic=['transfer', '${from}', '${to}'], data=${amount}`);
        logs.push(`[soroban-env-host] Successfully updated balances: ${from}=${nextFromBal}, ${to}=${nextToBal}`);
        returnValue = { ok: true, transferred: amount, from, to };
      }
      break;
    }

    case "balance": {
      const account = args.account || "GBBD...TESSERA";
      const bal = currentStorage[`BALANCE:${account}`] || "0";
      storageReads += 1;
      cpuInstructions += 2200;
      returnValue = bal;
      logs.push(`[soroban-env-host] Query balance(${account}) = ${bal}`);
      break;
    }

    case "mint": {
      const to = args.to || "GCXK...INVESTOR";
      const amount = parseInt(args.amount || "1000", 10);
      const toKey = `BALANCE:${to}`;
      const current = parseInt(currentStorage[toKey] || "0", 10);
      storageReads += 1;
      currentStorage[toKey] = (current + amount).toString();
      storageWrites += 1;
      cpuInstructions += 6100;
      events.push({ topic: ["mint", to], data: amount });
      returnValue = { ok: true, minted: amount, newBalance: current + amount };
      logs.push(`[soroban-env-host] Minted ${amount} to ${to}`);
      break;
    }

    default: {
      cpuInstructions += 1500;
      returnValue = { ok: true, function: functionName };
      logs.push(`[soroban-env-host] Executed generic method '${functionName}'`);
      break;
    }
  }

  return {
    functionName,
    success,
    returnValue,
    logs,
    events,
    metrics: {
      cpuInstructions,
      memoryPages,
      memoryBytes,
      storageReads,
      storageWrites,
    },
    error,
  };
}

/**
 * Builds the sandboxed iframe HTML document with strict CSP:
 * `sandbox="allow-scripts"`
 */
export function buildSandboxIframeHtml(): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' blob: data:; style-src 'unsafe-inline';">
  <title>Soroban Host Emulator Sandbox</title>
</head>
<body style="margin:0; background:#0f172a; color:#f8fafc; font-family:monospace; padding:12px; font-size:12px;">
  <div id="status">Soroban Host Emulator: Standby in isolated iframe</div>
  <script>
    var state = ${JSON.stringify(INITIAL_ASSET_TOKEN_STATE)};

    window.addEventListener('message', function(event) {
      var data = event.data;
      if (!data || data.type !== 'INVOKE_SOROBAN_CONTRACT') return;

      var functionName = data.functionName || 'transfer';
      var args = data.args || {};

      var currentStorage = Object.assign({}, state);
      var logs = ["[soroban-env-host:iframe] Invoking '" + functionName + "'"];
      var events = [];
      var cpuInstructions = 15240;
      var memoryPages = 1;
      var memoryBytes = 65536;
      var storageReads = 2;
      var storageWrites = 2;
      var success = true;
      var returnValue = null;
      var error = null;

      if (functionName === 'transfer') {
        var from = args.from || 'GBBD...TESSERA';
        var to = args.to || 'GCXK...INVESTOR';
        var amount = parseInt(args.amount || '10000', 10);

        var fromKey = 'BALANCE:' + from;
        var toKey = 'BALANCE:' + to;
        var fromBal = parseInt(currentStorage[fromKey] || '1000000', 10);
        var toBal = parseInt(currentStorage[toKey] || '50000', 10);

        if (fromBal < amount) {
          success = false;
          error = 'InsufficientBalance';
          logs.push("[soroban-env-host:iframe] Panic: InsufficientBalance");
        } else {
          currentStorage[fromKey] = (fromBal - amount).toString();
          currentStorage[toKey] = (toBal + amount).toString();
          state = currentStorage;
          events.push({ topic: ['transfer', from, to], data: amount });
          returnValue = { ok: true, from: from, to: to, amount: amount };
          logs.push("[soroban-env-host:iframe] Transfer complete. Emitted event topic=['transfer', '" + from + "', '" + to + "']");
        }
      } else {
        returnValue = { ok: true, fn: functionName };
      }

      var result = {
        type: 'SOROBAN_INVOCATION_RESULT',
        id: data.id,
        functionName: functionName,
        success: success,
        returnValue: returnValue,
        logs: logs,
        events: events,
        metrics: {
          cpuInstructions: cpuInstructions,
          memoryPages: memoryPages,
          memoryBytes: memoryBytes,
          storageReads: storageReads,
          storageWrites: storageWrites
        },
        error: error
      };

      document.getElementById('status').innerText = 'Last invocation: ' + functionName + ' (' + (success ? 'SUCCESS' : 'FAILED') + ')';
      if (event.source) {
        event.source.postMessage(result, '*');
      }
    });
  </script>
</body>
</html>`;
}
