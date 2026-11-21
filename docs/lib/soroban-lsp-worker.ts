/**
 * Custom LSP Server for Soroban Rust syntax highlighting and autocomplete.
 * Runs as a Web Worker or inline language service for Monaco Editor.
 */

export interface CompletionItem {
  label: string;
  kind: "keyword" | "function" | "struct" | "macro" | "snippet" | "type";
  detail: string;
  documentation: string;
  insertText: string;
}

export interface DiagnosticItem {
  line: number;
  column: number;
  severity: "error" | "warning" | "info";
  message: string;
}

export const SOROBAN_COMPLETIONS: CompletionItem[] = [
  {
    label: "#[contract]",
    kind: "keyword",
    detail: "Soroban contract attribute",
    documentation: "Declares a Rust struct as a Soroban smart contract.",
    insertText: "#[contract]\npub struct ${1:ContractName};\n",
  },
  {
    label: "#[contractimpl]",
    kind: "keyword",
    detail: "Soroban contract implementation",
    documentation: "Exports the functions inside the impl block as callable contract methods.",
    insertText: "#[contractimpl]\nimpl ${1:ContractName} {\n    $0\n}\n",
  },
  {
    label: "#[contracttype]",
    kind: "keyword",
    detail: "Soroban custom data type",
    documentation: "Derives Soroban serialization for structs and enums.",
    insertText: "#[contracttype]\n#[derive(Clone, Debug, Eq, PartialEq)]\npub struct ${1:TypeName} {\n    $0\n}\n",
  },
  {
    label: "#[contracterror]",
    kind: "keyword",
    detail: "Soroban contract error enum",
    documentation: "Defines an error enum with u32 codes for contract panics.",
    insertText: "#[contracterror]\n#[derive(Copy, Clone, Debug, Eq, PartialEq)]\n#[repr(u32)]\npub enum Error {\n    ${1:CustomError} = 1,\n}\n",
  },
  {
    label: "Env",
    kind: "type",
    detail: "soroban_sdk::Env",
    documentation: "The Soroban execution environment providing access to storage, events, auth, and ledger state.",
    insertText: "Env",
  },
  {
    label: "Address",
    kind: "type",
    detail: "soroban_sdk::Address",
    documentation: "Represents a Stellar account or contract address.",
    insertText: "Address",
  },
  {
    label: "Symbol",
    kind: "type",
    detail: "soroban_sdk::Symbol",
    documentation: "A small string up to 32 characters stored as a 64-bit integer.",
    insertText: "Symbol",
  },
  {
    label: "symbol_short!",
    kind: "macro",
    detail: "macro symbol_short!(str)",
    documentation: "Creates a compile-time validated short symbol up to 9 characters.",
    insertText: 'symbol_short!("${1:name}")',
  },
  {
    label: "panic_with_error!",
    kind: "macro",
    detail: "macro panic_with_error!(env, error)",
    documentation: "Halts execution with a specific Soroban contract error.",
    insertText: "panic_with_error!(env, ${1:Error::CustomError});",
  },
  {
    label: "env.storage().instance().set",
    kind: "function",
    detail: "env.storage().instance().set(&key, &value)",
    documentation: "Stores a key-value pair in instance storage.",
    insertText: "env.storage().instance().set(&${1:key}, &${2:value});",
  },
  {
    label: "env.storage().instance().get",
    kind: "function",
    detail: "env.storage().instance().get(&key)",
    documentation: "Retrieves a value from instance storage.",
    insertText: "env.storage().instance().get(&${1:key})",
  },
  {
    label: "env.storage().persistent().set",
    kind: "function",
    detail: "env.storage().persistent().set(&key, &value)",
    documentation: "Stores a key-value pair in persistent storage.",
    insertText: "env.storage().persistent().set(&${1:key}, &${2:value});",
  },
  {
    label: "env.storage().persistent().get",
    kind: "function",
    detail: "env.storage().persistent().get(&key)",
    documentation: "Retrieves a value from persistent storage.",
    insertText: "env.storage().persistent().get(&${1:key})",
  },
  {
    label: "env.events().publish",
    kind: "function",
    detail: "env.events().publish(topics, data)",
    documentation: "Emits a contract event with up to 4 topics and payload.",
    insertText: "env.events().publish((${1:topics}), ${2:data});",
  },
  {
    label: "address.require_auth()",
    kind: "function",
    detail: "address.require_auth()",
    documentation: "Verifies cryptographic signature authorization for the given address.",
    insertText: "${1:from}.require_auth();",
  },
];

export function getCompletions(prefix: string): CompletionItem[] {
  const query = prefix.toLowerCase();
  if (!query) return SOROBAN_COMPLETIONS;
  return SOROBAN_COMPLETIONS.filter(
    (item) =>
      item.label.toLowerCase().includes(query) ||
      item.detail.toLowerCase().includes(query)
  );
}

export function validateSorobanRust(source: string): DiagnosticItem[] {
  const diagnostics: DiagnosticItem[] = [];
  const lines = source.split("\n");

  if (!source.includes("#![no_std]") && !source.includes("#![no_std]")) {
    diagnostics.push({
      line: 1,
      column: 1,
      severity: "warning",
      message: "Soroban contracts should declare #![no_std] for WASM compatibility.",
    });
  }

  if (!source.includes("soroban_sdk")) {
    diagnostics.push({
      line: 1,
      column: 1,
      severity: "info",
      message: "Import soroban_sdk::{contract, contractimpl, Env, ...} to use Soroban types.",
    });
  }

  if (source.includes("fn ") && !source.includes("#[contractimpl]")) {
    lines.forEach((line, index) => {
      if (line.includes("fn ") && !line.includes("fn main")) {
        diagnostics.push({
          line: index + 1,
          column: 1,
          severity: "info",
          message: "Contract methods must be inside an impl block annotated with #[contractimpl].",
        });
      }
    });
  }

  return diagnostics;
}

/**
 * Worker script string to initialize a Web Worker for LSP operations.
 */
export const LSP_WORKER_CODE = `
self.onmessage = function(e) {
  const { id, type, payload } = e.data;
  if (type === 'complete') {
    const { prefix } = payload || {};
    const query = (prefix || '').toLowerCase();
    const items = ${JSON.stringify(SOROBAN_COMPLETIONS)}.filter(item =>
      !query || item.label.toLowerCase().includes(query) || item.detail.toLowerCase().includes(query)
    );
    self.postMessage({ id, type: 'complete_result', items });
  } else if (type === 'validate') {
    const { source } = payload || {};
    const diagnostics = [];
    if (source && !source.includes('#![no_std]')) {
      diagnostics.push({ line: 1, column: 1, severity: 'warning', message: 'Soroban contracts require #![no_std]' });
    }
    self.postMessage({ id, type: 'validate_result', diagnostics });
  }
};
`;

/**
 * Creates a dedicated Web Worker instance for the Soroban Rust LSP server.
 */
export function createSorobanLspWorker(): Worker | null {
  if (typeof window === "undefined" || typeof Worker === "undefined") {
    return null;
  }
  try {
    const blob = new Blob([LSP_WORKER_CODE], { type: "application/javascript" });
    const url = URL.createObjectURL(blob);
    return new Worker(url);
  } catch {
    return null;
  }
}
