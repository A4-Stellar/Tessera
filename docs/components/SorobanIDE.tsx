"use client";

import dynamic from "next/dynamic";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { EditorProps } from "@monaco-editor/react";

export interface SorobanIDEProps {
  compilerEndpoint?: string;
  sandboxEndpoint?: string;
  sandboxUrl?: string;
  compileEndpoint?: string;
  rpcUrl?: string;
  testnetRpcUrl?: string;
  networkPassphrase?: string;
  initialCode?: string;
  title?: string;
  className?: string;
}

type CompileState = "idle" | "loading" | "success" | "error";
type WalletState =
  | "checking"
  | "unavailable"
  | "disconnected"
  | "connecting"
  | "connected"
  | "signing"
  | "deploying"
  | "error";

type CompilerPayload = Record<string, unknown>;

export interface CompileArtifact {
  wasm?: string;
  transactionXdr?: string;
  contractId?: string;
  warnings: string[];
  logs: string[];
}

type FreighterResponse = Record<string, unknown>;

type FreighterLike = {
  isConnected?: () => Promise<unknown>;
  getAddress?: () => Promise<unknown>;
  getNetwork?: () => Promise<unknown>;
  getNetworkDetails?: () => Promise<unknown>;
  requestAccess?: () => Promise<unknown>;
  signTransaction?: (
    transactionXdr: string,
    options?: { networkPassphrase?: string; address?: string }
  ) => Promise<unknown>;
};

type DeploymentResult = {
  hash: string;
  status: string;
};

type WalletSession = {
  api: FreighterLike;
  address: string;
  passphrase: string;
};

const DEFAULT_RPC_URL = "https://soroban-testnet.stellar.org";
const TESTNET_PASSPHRASE = "Test SDF Network ; September 2015";
const MAX_SOURCE_LENGTH = 100000;
const MAX_RESPONSE_LENGTH = 2000000;
const MAX_WASM_LENGTH = 4000000;
const MAX_XDR_LENGTH = 150000;
const DEFAULT_RUST_SOURCE = `#![no_std]
use soroban_sdk::{contract, contracttype, Env};

#[contract]
pub struct ProspectusRegistry;

#[contracttype]
#[derive(Clone)]
pub struct AssetRecord {
    pub asset_id: u32,
    pub issuer: String,
}

#[contractimpl]
impl ProspectusRegistry {
    pub fn register(env: Env, asset_id: u32, issuer: String) {
        env.storage().instance().set(&asset_id, &AssetRecord { asset_id, issuer });
    }
}`;

const LazyMonacoEditor = dynamic<EditorProps>(
  () => import("@monaco-editor/react").then((module) => module.default),
  {
    ssr: false,
    loading: () => (
      <div role="status" className="flex h-72 items-center justify-center rounded-lg border border-white/10 bg-base-900 text-sm text-base-300">
        Loading Rust editor…
      </div>
    ),
  }
);

function boundedString(value: unknown, maximum: number): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  if (!trimmed || trimmed.length > maximum || /[\u0000-\u001f\u007f]/.test(trimmed)) return null;
  return trimmed;
}

function recordValue(value: unknown): CompilerPayload | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as CompilerPayload)
    : null;
}

function errorValue(value: unknown): string | null {
  if (typeof value === "string") return boundedString(value, 600);
  const record = recordValue(value);
  if (!record) return null;
  return boundedString(record.message, 600) || boundedString(record.error, 600);
}

function compilerError(value: unknown): string | null {
  const record = recordValue(value);
  if (!record) return null;
  if (record.error !== undefined) return errorValue(record.error) || "The compiler reported an error.";
  if (record.success === false) return errorValue(record.message) || "The compiler reported an error.";
  return null;
}

function decodeHex(value: string): Uint8Array | null {
  if (!/^(?:0x)?[0-9a-f]+$/i.test(value) || value.replace(/^0x/i, "").length % 2 !== 0) return null;
  const normalized = value.replace(/^0x/i, "");
  const bytes = new Uint8Array(normalized.length / 2);
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(normalized.slice(index * 2, index * 2 + 2), 16);
  }
  return bytes;
}

function decodeBase64(value: string): Uint8Array | null {
  const normalized = value.replace(/\s/g, "");
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(normalized) || normalized.length % 4 === 1) return null;
  try {
    const binary = globalThis.atob(normalized);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
    return bytes;
  } catch {
    return null;
  }
}

function decodeWasm(value: string): Uint8Array {
  const compact = value.replace(/\s/g, "");
  if (compact.length > MAX_WASM_LENGTH * 2) {
    throw new Error("The compiler returned an oversized WASM artifact.");
  }
  const bytes = decodeHex(compact) || decodeBase64(compact);
  if (!bytes || bytes.byteLength === 0) {
    throw new Error("The compiler returned an invalid WASM artifact.");
  }
  if (bytes.byteLength < 4 || bytes[0] !== 0x00 || bytes[1] !== 0x61 || bytes[2] !== 0x73 || bytes[3] !== 0x6d) {
    throw new Error("The compiler returned an invalid WASM artifact.");
  }
  if (bytes.byteLength > MAX_WASM_LENGTH) {
    throw new Error("The compiler returned an oversized WASM artifact.");
  }
  return bytes;
}

function normalizeStrings(value: unknown, maximum: number, maximumItems: number): string[] {
  if (typeof value === "string") {
    const single = boundedString(value, maximum);
    return single ? [single] : [];
  }
  if (!Array.isArray(value)) return [];
  return value
    .slice(0, maximumItems)
    .map((item) => boundedString(item, maximum))
    .filter((item): item is string => Boolean(item));
}

export function isSecureEndpoint(value: string): boolean {
  if (!value.trim()) return false;
  try {
    const base = typeof window !== "undefined" ? window.location.origin : "https://docs.invalid";
    const endpoint = new URL(value, base);
    if (endpoint.username || endpoint.password) return false;
    if (endpoint.protocol === "https:") return true;
    return (
      endpoint.protocol === "http:" &&
      (endpoint.hostname === "localhost" || endpoint.hostname === "127.0.0.1" || endpoint.hostname === "[::1]")
    );
  } catch {
    return false;
  }
}

function resolvedEndpoint(value: string, label: string): string {
  if (!isSecureEndpoint(value)) {
    throw new Error(`${label} must use HTTPS or a local development origin.`);
  }
  const base = typeof window !== "undefined" ? window.location.origin : "https://docs.invalid";
  return new URL(value, base).toString();
}

function endpointHost(value: string): string {
  try {
    const base = typeof window !== "undefined" ? window.location.origin : "https://docs.invalid";
    return new URL(value, base).host;
  } catch {
    return "configured endpoint";
  }
}

function responseError(response: { ok?: boolean; status?: number; statusText?: string }): string | null {
  if (response.ok !== false) return null;
  const status = typeof response.status === "number" ? ` (${response.status})` : "";
  return `Request failed${status}${response.statusText ? `: ${response.statusText}` : ""}`;
}

async function readJsonResponse(response: Response): Promise<unknown> {
  const failure = responseError(response);
  if (failure) throw new Error(failure);
  let text = "";
  if (typeof response.text === "function") {
    const responseText = await response.text();
    text = typeof responseText === "string" ? responseText : "";
  }
  if (text.length > MAX_RESPONSE_LENGTH) {
    throw new Error("The compiler response was too large.");
  }
  if (text) {
    try {
      return JSON.parse(text) as unknown;
    } catch {
      throw new Error("The compiler returned invalid JSON.");
    }
  }
  if (typeof response.json === "function") {
    const payload = await response.json();
    if (typeof payload === "string" && payload.length > MAX_RESPONSE_LENGTH) {
      throw new Error("The compiler response was too large.");
    }
    return payload;
  }
  throw new Error("The compiler returned an empty response.");
}

function unwrapPayload(value: unknown): unknown {
  const record = recordValue(value);
  if (!record) return value;
  if ("data" in record) return record.data;
  if ("result" in record && record.result !== undefined) return record.result;
  return record;
}

export function normalizeCompilerResponse(value: unknown): CompileArtifact {
  const outerError = compilerError(value);
  if (outerError) throw new Error(outerError);

  const unwrapped = unwrapPayload(value);
  if (typeof unwrapped === "string") {
    const wasm = boundedString(unwrapped, MAX_WASM_LENGTH * 2);
    if (!wasm) throw new Error("The compiler returned an empty artifact.");
    decodeWasm(wasm);
    return { wasm, warnings: [], logs: [] };
  }
  const record = recordValue(unwrapped);
  if (!record) throw new Error("The compiler returned an invalid response.");
  const responseErrorMessage = compilerError(record);
  if (responseErrorMessage) throw new Error(responseErrorMessage);

  const deployment = recordValue(record.deployment);
  const artifact = deployment || record;
  const transactionXdr =
    boundedString(artifact.transactionXdr, MAX_XDR_LENGTH) ||
    boundedString(artifact.transaction_xdr, MAX_XDR_LENGTH) ||
    boundedString(artifact.deploymentTransactionXdr, MAX_XDR_LENGTH) ||
    boundedString(artifact.deployTransactionXdr, MAX_XDR_LENGTH) ||
    boundedString(artifact.xdr, MAX_XDR_LENGTH) ||
    boundedString(record.transactionXdr, MAX_XDR_LENGTH) ||
    boundedString(record.transaction_xdr, MAX_XDR_LENGTH);
  const wasmValue =
    boundedString(artifact.wasm, MAX_WASM_LENGTH * 2) ||
    boundedString(artifact.wasmBase64, MAX_WASM_LENGTH * 2) ||
    boundedString(artifact.wasm_base64, MAX_WASM_LENGTH * 2) ||
    boundedString(artifact.contractWasm, MAX_WASM_LENGTH * 2) ||
    boundedString(record.wasm, MAX_WASM_LENGTH * 2) ||
    boundedString(record.wasmBase64, MAX_WASM_LENGTH * 2) ||
    boundedString(record.wasm_base64, MAX_WASM_LENGTH * 2);
  if (!transactionXdr && !wasmValue) {
    throw new Error("The compiler response did not include WASM or a deployment transaction.");
  }
  if (wasmValue) decodeWasm(wasmValue);
  const contractId =
    boundedString(artifact.contractId, 160) ||
    boundedString(artifact.contract_id, 160) ||
    boundedString(record.contractId, 160) ||
    boundedString(record.contract_id, 160) ||
    undefined;
  return {
    wasm: wasmValue || undefined,
    transactionXdr: transactionXdr || undefined,
    contractId,
    warnings: normalizeStrings(artifact.warnings ?? record.warnings, 600, 20),
    logs: normalizeStrings(artifact.logs ?? record.logs, 1000, 20),
  };
}

function isFreighterLike(value: unknown): value is FreighterLike {
  const record = recordValue(value);
  return Boolean(
    record &&
      (typeof record.getAddress === "function" || typeof record.requestAccess === "function") &&
      typeof record.signTransaction === "function"
  );
}

function injectedFreighter(): FreighterLike | null {
  if (typeof window === "undefined") return null;
  const browserWindow = window as Window & { freighter?: unknown };
  return isFreighterLike(browserWindow.freighter) ? browserWindow.freighter : null;
}

async function loadFreighter(): Promise<FreighterLike> {
  const injected = injectedFreighter();
  if (injected) return injected;
  const freighterModule = await import("@stellar/freighter-api");
  const candidate = freighterModule.default || freighterModule;
  if (!isFreighterLike(candidate)) {
    throw new Error("Freighter is not available in this browser.");
  }
  return candidate;
}

function freighterError(value: unknown): string | null {
  const record = recordValue(value);
  if (!record) return null;
  return errorValue(record.error) || (record.success === false ? errorValue(record.message) : null);
}

function responseAddress(value: unknown): string {
  const direct = boundedString(value, 160);
  if (direct) return direct;
  const record = recordValue(value);
  if (!record) throw new Error("Freighter did not return a valid account address.");
  const address = boundedString(record.address, 160);
  if (!address) throw new Error("Freighter did not return a valid account address.");
  return address;
}

function signedTransaction(value: unknown): string {
  const direct = boundedString(value, MAX_XDR_LENGTH);
  if (direct) return direct;
  const record = recordValue(value);
  if (!record) throw new Error("Freighter did not return a signed transaction.");
  const signed = boundedString(record.signedTxXdr, MAX_XDR_LENGTH) || boundedString(record.signedTransactionXdr, MAX_XDR_LENGTH);
  if (!signed) throw new Error("Freighter did not return a signed transaction.");
  return signed;
}

async function requestAddress(api: FreighterLike): Promise<string> {
  const request = api.requestAccess || api.getAddress;
  if (!request) throw new Error("This Freighter version cannot provide an account address.");
  const response = await request.call(api);
  const responseMessage = freighterError(response);
  if (responseMessage) throw new Error(responseMessage);
  return responseAddress(response);
}

async function requestNetwork(api: FreighterLike, expectedPassphrase: string): Promise<string> {
  const request = api.getNetwork || api.getNetworkDetails;
  if (!request) throw new Error("This Freighter version cannot verify the Testnet network.");
  const response = await request.call(api);
  const responseMessage = freighterError(response);
  if (responseMessage) throw new Error(responseMessage);
  const record = recordValue(response);
  if (!record) throw new Error("Freighter returned an invalid network response.");
  const network = boundedString(record.network, 120);
  const passphrase =
    boundedString(record.networkPassphrase, 300) || boundedString(record.passphrase, 300);
  if (!network && !passphrase) {
    throw new Error("Freighter did not return network information.");
  }
  if (!network || !/testnet|test sdf/i.test(network)) {
    throw new Error("Freighter must be switched to Testnet before deployment.");
  }
  if (!passphrase || passphrase !== expectedPassphrase) {
    throw new Error("Freighter is connected to a different network passphrase.");
  }
  return passphrase;
}

function rpcResult(value: unknown): DeploymentResult {
  const root = recordValue(value);
  if (!root) throw new Error("The Testnet RPC returned an invalid response.");
  const rpcError = errorValue(root.error);
  if (rpcError) throw new Error(rpcError);
  const result = recordValue(root.result) || root;
  const hash = boundedString(result.hash, 200) || boundedString(result.txHash, 200) || boundedString(result.transactionHash, 200);
  if (!hash) throw new Error("The Testnet RPC did not return a transaction hash.");
  const status = boundedString(result.status, 80) || "SUBMITTED";
  if (/error|failed|rejected/i.test(status)) {
    throw new Error(`The Testnet RPC rejected the transaction: ${status}.`);
  }
  return { hash, status };
}

async function submitSignedTransaction(
  signedXdr: string,
  rpcUrl: string,
  signal?: AbortSignal
): Promise<DeploymentResult> {
  const response = await fetch(rpcUrl, {
    method: "POST",
    credentials: "omit",
    cache: "no-store",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "sendTransaction",
      params: { tx: signedXdr },
    }),
    signal,
  });
  return rpcResult(await readJsonResponse(response));
}

async function buildUploadTransaction(
  wasm: string,
  address: string,
  rpcUrl: string,
  passphrase: string
): Promise<string> {
  const sdk = await import("@stellar/stellar-sdk");
  const wasmBytes = decodeWasm(wasm);
  const server = new sdk.rpc.Server(rpcUrl, { allowHttp: rpcUrl.startsWith("http://") });
  const account = await server.getAccount(address);
  const transaction = new sdk.TransactionBuilder(account, {
    fee: sdk.BASE_FEE,
    networkPassphrase: passphrase,
  })
    .setTimeout(300)
    .addOperation(sdk.Operation.uploadContractWasm({ wasm: wasmBytes }))
    .build();
  const prepared = await server.prepareTransaction(transaction);
  return prepared.toXDR();
}

function timeoutSignal(timeoutMs: number): { controller: AbortController; signal: AbortSignal; cancel: () => void } {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  return { controller, signal: controller.signal, cancel: () => clearTimeout(timeout) };
}

export function SorobanIDE({
  compilerEndpoint,
  sandboxEndpoint,
  sandboxUrl,
  compileEndpoint,
  rpcUrl,
  testnetRpcUrl,
  networkPassphrase = TESTNET_PASSPHRASE,
  initialCode = DEFAULT_RUST_SOURCE,
  title = "Soroban Rust IDE",
  className = "",
}: SorobanIDEProps) {
  const editorDescriptionId = useId();
  const [code, setCode] = useState(initialCode);
  const [compileState, setCompileState] = useState<CompileState>("idle");
  const [compileMessage, setCompileMessage] = useState("Ready to compile in the configured sandbox.");
  const [warnings, setWarnings] = useState<string[]>([]);
  const [logs, setLogs] = useState<string[]>([]);
  const [artifact, setArtifact] = useState<CompileArtifact | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [walletState, setWalletState] = useState<WalletState>("checking");
  const [walletAddress, setWalletAddress] = useState("");
  const [deployment, setDeployment] = useState<DeploymentResult | null>(null);
  const [deployMessage, setDeployMessage] = useState("");
  const actionAbortRef = useRef<AbortController | null>(null);
  const compilerEndpointValue =
    compilerEndpoint || sandboxEndpoint || sandboxUrl || compileEndpoint || process.env.NEXT_PUBLIC_SOROBAN_SANDBOX_URL || "";
  const rpcUrlValue = rpcUrl || testnetRpcUrl || DEFAULT_RPC_URL;
  const endpointIsConfigured = Boolean(compilerEndpointValue);
  const endpointIsValid = !endpointIsConfigured || isSecureEndpoint(compilerEndpointValue);
  const rpcIsValid = isSecureEndpoint(rpcUrlValue);
  const networkPassphraseIsValid = networkPassphrase === TESTNET_PASSPHRASE;
  const busy = compileState === "loading" || walletState === "connecting" || walletState === "signing" || walletState === "deploying";

  useEffect(() => {
    const injected = injectedFreighter();
    if (injected) {
      setWalletState("disconnected");
    } else {
      setWalletState("unavailable");
    }
  }, []);

  useEffect(() => {
    return () => {
      actionAbortRef.current?.abort();
    };
  }, []);

  function setSource(value: string | undefined) {
    setCode(value ?? "");
    setArtifact(null);
    setDeployment(null);
    setWarnings([]);
    setLogs([]);
    setCompileState("idle");
    setCompileMessage("Ready to compile in the configured sandbox.");
    setError(null);
    setDeployMessage("");
  }

  async function connectWallet(): Promise<WalletSession | null> {
    setWalletState("connecting");
    setError(null);
    try {
      const api = await loadFreighter();
      const address = await requestAddress(api);
      const passphrase = await requestNetwork(api, TESTNET_PASSPHRASE);
      setWalletAddress(address);
      setWalletState("connected");
      setDeployMessage(`Wallet connected: ${address}`);
      return { api, address, passphrase };
    } catch (walletFailure: unknown) {
      setWalletState("error");
      setError(walletFailure instanceof Error ? walletFailure.message : "Freighter could not be connected.");
      return null;
    }
  }

  async function compileSource() {
    setError(null);
    setDeployment(null);
    setDeployMessage("");
    if (!endpointIsConfigured) {
      setCompileState("error");
      setCompileMessage("Configure a secure compiler endpoint before compiling.");
      return;
    }
    if (!endpointIsValid) {
      setCompileState("error");
      setCompileMessage("The compiler endpoint must use HTTPS or a local development origin.");
      return;
    }
    if (!code.trim()) {
      setCompileState("error");
      setCompileMessage("Enter Rust source before compiling.");
      return;
    }
    if (code.length > MAX_SOURCE_LENGTH) {
      setCompileState("error");
      setCompileMessage("The Rust source is too large for the sandbox.");
      return;
    }
    let endpoint: string;
    try {
      endpoint = resolvedEndpoint(compilerEndpointValue, "Compiler endpoint");
    } catch (endpointFailure: unknown) {
      setCompileState("error");
      setCompileMessage(endpointFailure instanceof Error ? endpointFailure.message : "The compiler endpoint is invalid.");
      return;
    }

    setCompileState("loading");
    setCompileMessage("Compiling Rust in the secure sandbox…");
    setWarnings([]);
    setLogs([]);
    const timeout = timeoutSignal(30000);
    actionAbortRef.current = timeout.controller;
    const controller = timeout.controller;
    try {
      const response = await fetch(endpoint, {
        method: "POST",
        credentials: "omit",
        cache: "no-store",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ language: "rust", source: code, network: "testnet" }),
        signal: controller.signal,
      });
      const payload = await readJsonResponse(response);
      const nextArtifact = normalizeCompilerResponse(payload);
      setArtifact(nextArtifact);
      setWarnings(nextArtifact.warnings);
      setLogs(nextArtifact.logs);
      setCompileState("success");
      setCompileMessage(
        nextArtifact.transactionXdr
          ? "Compilation succeeded. The sandbox prepared a Testnet deployment transaction."
          : "Compilation succeeded. The sandbox returned WASM for wallet-signed upload."
      );
    } catch (compileFailure: unknown) {
      if (controller.signal.aborted) {
        setCompileState("error");
        setCompileMessage("The compiler request timed out or was cancelled.");
      } else {
        setCompileState("error");
        setCompileMessage(
          compileFailure instanceof Error ? compileFailure.message : "The secure compiler request failed."
        );
      }
    } finally {
      timeout.cancel();
      actionAbortRef.current = null;
    }
  }

  async function deployArtifact() {
    if (!artifact) {
      setError("Compile the contract before deploying.");
      return;
    }
    if (!networkPassphraseIsValid) {
      setError("The IDE is restricted to Stellar Testnet.");
      return;
    }
    if (!rpcIsValid) {
      setError("The Testnet RPC endpoint must use HTTPS or a local development origin.");
      return;
    }
    setError(null);
    setDeployment(null);
    let rpcEndpoint: string;
    try {
      rpcEndpoint = resolvedEndpoint(rpcUrlValue, "Testnet RPC endpoint");
    } catch (rpcFailure: unknown) {
      setError(rpcFailure instanceof Error ? rpcFailure.message : "The Testnet RPC endpoint is invalid.");
      return;
    }
    let session: WalletSession | null;
    try {
      session = walletAddress
        ? await (async () => {
            const api = await loadFreighter();
            const passphrase = await requestNetwork(api, TESTNET_PASSPHRASE);
            return { api, address: walletAddress, passphrase };
          })()
        : await connectWallet();
    } catch {
      return;
    }
    if (!session) return;

    setWalletState("signing");
    setDeployMessage("Review the deployment transaction in Freighter.");
    const timeout = timeoutSignal(60000);
    actionAbortRef.current = timeout.controller;
    try {
      const transactionXdr =
        artifact.transactionXdr ||
        (artifact.wasm
          ? await buildUploadTransaction(artifact.wasm, session.address, rpcEndpoint, session.passphrase)
          : "");
      if (!transactionXdr) throw new Error("The compiler did not return a deployable transaction.");
      if (!session.api.signTransaction) throw new Error("This Freighter version cannot sign transactions.");
      const signedResponse = await session.api.signTransaction(transactionXdr, {
        networkPassphrase: session.passphrase,
        address: session.address,
      });
      const signedMessage = freighterError(signedResponse);
      if (signedMessage) throw new Error(signedMessage);
      const signed = signedTransaction(signedResponse);
      setWalletState("deploying");
      setDeployMessage("Submitting the signed transaction to Stellar Testnet…");
      const submitted = await submitSignedTransaction(signed, rpcEndpoint, timeout.signal);
      setDeployment(submitted);
      setWalletState("connected");
      setDeployMessage(`Transaction ${submitted.status.toLowerCase()} on Testnet: ${submitted.hash}`);
    } catch (deploymentFailure: unknown) {
      setWalletState("error");
      if (timeout.signal.aborted) {
        setError("The Testnet deployment request timed out or was cancelled.");
      } else {
        setError(deploymentFailure instanceof Error ? deploymentFailure.message : "The Testnet deployment failed.");
      }
    } finally {
      timeout.cancel();
      actionAbortRef.current = null;
    }
  }

  const walletLabel = useMemo(() => {
    switch (walletState) {
      case "checking":
        return "Checking for Freighter…";
      case "unavailable":
        return "Freighter was not detected; use Connect wallet to retry.";
      case "disconnected":
        return "Freighter is available but no wallet session is connected.";
      case "connecting":
        return "Connecting to Freighter…";
      case "connected":
        return `Connected to ${walletAddress}`;
      case "signing":
        return "Waiting for Freighter signature…";
      case "deploying":
        return "Submitting deployment…";
      case "error":
        return "The wallet session needs attention.";
      default:
        return "Wallet status unavailable.";
    }
  }, [walletAddress, walletState]);

  return (
    <section
      className={`my-6 overflow-hidden rounded-xl border border-white/10 bg-white/[0.03] ${className}`}
      aria-labelledby={`${editorDescriptionId}-title`}
      aria-busy={busy}
      data-testid="soroban-ide"
    >
      <header className="flex flex-wrap items-start justify-between gap-3 border-b border-white/10 px-4 py-3">
        <div>
          <h3 id={`${editorDescriptionId}-title`} className="text-base font-bold text-base-50">
            {title}
          </h3>
          <p className="mt-1 text-xs text-base-300">Rust is compiled by a remote sandbox, never executed in this page.</p>
        </div>
        <span className="rounded-full border border-gold-500/30 bg-gold-500/10 px-2.5 py-1 text-xs font-semibold text-gold-300">
          Testnet only
        </span>
      </header>

      <div className="space-y-4 p-4">
        <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-base-300">
          <span>
            Compiler: {endpointIsConfigured ? endpointHost(compilerEndpointValue) : "not configured"}
          </span>
          <span>RPC: {rpcIsValid ? endpointHost(rpcUrlValue) : "invalid endpoint"}</span>
        </div>
        {!endpointIsValid && (
          <p role="alert" className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            The compiler endpoint must use HTTPS or a local development origin.
          </p>
        )}
        {!rpcIsValid && (
          <p role="alert" className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            The Testnet RPC endpoint must use HTTPS or a local development origin.
          </p>
        )}
        {!networkPassphraseIsValid && (
          <p role="alert" className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            The IDE only supports the Stellar Testnet passphrase.
          </p>
        )}

        <div>
          <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
            <label htmlFor={`${editorDescriptionId}-source`} className="text-sm font-semibold text-base-100">
              Rust source
            </label>
            <span className="text-xs text-base-400">Language: Rust</span>
          </div>
          <p id={editorDescriptionId} className="mb-2 text-xs text-base-300">
            Edit <code className="text-brand-300">contract.rs</code>. Compilation sends the source to the configured sandbox.
          </p>
          <div className="overflow-hidden rounded-lg border border-white/10" aria-label="Rust source editor">
            <LazyMonacoEditor
              height="360px"
              language="rust"
              theme="vs-dark"
              path="contract.rs"
              value={code}
              onChange={setSource}
              loading={
                <div role="status" className="flex h-72 items-center justify-center text-sm text-base-300">
                  Loading Rust editor…
                </div>
              }
              options={{
                automaticLayout: true,
                minimap: { enabled: false },
                scrollBeyondLastLine: false,
                fontSize: 14,
                accessibilitySupport: "on",
              }}
            />
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={compileSource}
            disabled={busy || !endpointIsConfigured || !endpointIsValid}
            className="rounded-md bg-brand-500 px-4 py-2 text-sm font-bold text-base-950 hover:bg-brand-400 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-300 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {compileState === "loading" ? "Compiling…" : "Compile Rust"}
          </button>
          <button
            type="button"
            onClick={connectWallet}
            disabled={busy || walletState === "connected"}
            className="rounded-md border border-white/10 px-3 py-2 text-sm font-medium text-base-100 hover:border-brand-500/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {walletState === "connected" ? "Wallet connected" : "Connect wallet"}
          </button>
          {artifact && (
            <button
              type="button"
              onClick={deployArtifact}
              disabled={busy || !networkPassphraseIsValid || !rpcIsValid}
              className="rounded-md border border-brand-500/50 bg-brand-500/10 px-3 py-2 text-sm font-semibold text-brand-300 hover:bg-brand-500/20 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-300 disabled:cursor-not-allowed disabled:opacity-50"
            >
              Deploy to Testnet
            </button>
          )}
        </div>

        <div className="grid gap-3 md:grid-cols-2">
          <div className="rounded-lg border border-white/10 bg-base-900/60 p-3">
            <p className="text-xs font-semibold uppercase tracking-wide text-base-300">Compiler status</p>
            <p role="status" aria-live="polite" className="mt-2 text-sm text-base-100">
              {compileMessage}
            </p>
            {artifact?.contractId && (
              <p className="mt-2 break-all font-mono text-xs text-brand-300">Contract: {artifact.contractId}</p>
            )}
            {artifact?.wasm && (
              <p className="mt-1 text-xs text-base-300">WASM artifact: {decodeWasm(artifact.wasm).byteLength.toLocaleString()} bytes</p>
            )}
          </div>
          <div className="rounded-lg border border-white/10 bg-base-900/60 p-3">
            <p className="text-xs font-semibold uppercase tracking-wide text-base-300">Wallet status</p>
            <p role="status" aria-live="polite" className="mt-2 break-all text-sm text-base-100">
              {walletLabel}
            </p>
            {walletAddress && <p className="mt-2 break-all font-mono text-xs text-brand-300">{walletAddress}</p>}
            {deployMessage && <p className="mt-2 text-xs text-base-300">{deployMessage}</p>}
            {deployment && <p className="mt-2 break-all font-mono text-xs text-emerald-300">Transaction: {deployment.hash}</p>}
          </div>
        </div>

        {error && (
          <div role="alert" className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300">
            {error}
          </div>
        )}

        {warnings.length > 0 && (
          <div className="rounded-md border border-gold-500/30 bg-gold-500/10 p-3 text-xs text-gold-300">
            <p className="font-semibold">Compiler warnings</p>
            <ul className="mt-1 list-disc space-y-1 pl-4">
              {warnings.map((warning, index) => <li key={`${warning}-${index}`}>{warning}</li>)}
            </ul>
          </div>
        )}

        {logs.length > 0 && (
          <details className="rounded-md border border-white/10 bg-base-950 p-3 text-xs text-base-300">
            <summary className="cursor-pointer font-semibold text-base-100">Sandbox output</summary>
            <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-words font-mono">{logs.join("\n")}</pre>
          </details>
        )}

        <p className="text-xs text-base-400">
          The sandbox must return a deployment transaction or WASM artifact. Rust code is never evaluated, and private keys never enter this component.
        </p>
      </div>
    </section>
  );
}

export default SorobanIDE;
