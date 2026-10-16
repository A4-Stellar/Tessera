/**
 * Unified wallet adapter suite for the Tessera docs app.
 *
 * Provides a single `WalletAdapter` interface across Freighter, Albedo, Rabet,
 * xBull and LOBSTR, with synchronous injected-global detection and a DOM-based
 * fallback install modal (this file is `.ts`, so the modal uses raw DOM APIs
 * instead of JSX).
 */

export type WalletId = "freighter" | "albedo" | "rabet" | "xbull" | "lobstr";

export interface WalletSignOptions {
  networkPassphrase?: string;
  network?: string;
  address?: string;
}

export interface WalletAdapter {
  readonly id: WalletId;
  readonly name: string;
  readonly installUrl: string;
  isInstalled(): boolean;
  connect(): Promise<string>;
  disconnect(): Promise<void>;
  getPublicKey(): string | null;
  signTransaction(xdr: string, options?: WalletSignOptions): Promise<string>;
}

export class WalletNotInstalledError extends Error {
  readonly walletId: WalletId | null;

  constructor(walletId: WalletId | null = null, message?: string) {
    super(message ?? `${walletId ?? "The selected"} wallet is not installed`);
    this.name = "WalletNotInstalledError";
    this.walletId = walletId;
  }
}

export const WALLET_OPTIONS: ReadonlyArray<{
  id: WalletId;
  name: string;
  installUrl: string;
}> = [
  { id: "freighter", name: "Freighter", installUrl: "https://www.freighter.app/" },
  { id: "albedo", name: "Albedo", installUrl: "https://albedo.link/" },
  { id: "rabet", name: "Rabet", installUrl: "https://rabet.io/" },
  { id: "xbull", name: "xBull", installUrl: "https://xbull.app/" },
  { id: "lobstr", name: "LOBSTR", installUrl: "https://lobstr.co/" },
] as const;

type UnknownRecord = Record<string, unknown>;

function browserWindow(): (Window & Record<string, unknown>) | null {
  return typeof window === "undefined"
    ? null
    : (window as unknown as Window & Record<string, unknown>);
}

function isRecord(value: unknown): value is UnknownRecord {
  return typeof value === "object" && value !== null;
}

function errorMessage(error: unknown, fallback: string): string {
  if (typeof error === "string" && error) return error;
  if (isRecord(error)) {
    if (typeof error.message === "string" && error.message) return error.message;
    if (isRecord(error.error)) {
      const nested = error.error.message;
      if (typeof nested === "string" && nested) return nested;
    }
    if (typeof error.error === "string" && error.error) return error.error;
  }
  if (error instanceof Error && error.message) return error.message;
  return fallback;
}

function pickString(source: UnknownRecord | null | undefined, keys: string[]): string {
  if (!source) return "";
  for (const key of keys) {
    const value = source[key];
    if (typeof value === "string" && value) return value;
  }
  return "";
}

function extractError(result: unknown): string | null {
  if (!isRecord(result)) return null;
  const error = result.error;
  if (!error) return null;
  if (typeof error === "string") return error || null;
  if (isRecord(error)) {
    const message = error.message;
    if (typeof message === "string" && message) return message;
    return "The wallet reported an error";
  }
  return null;
}

type FreighterLike = {
  isConnected?: () => Promise<unknown>;
  requestAccess?: () => Promise<unknown>;
  getAddress?: () => Promise<unknown>;
  signTransaction?: (xdr: string, options?: UnknownRecord) => Promise<unknown>;
  disconnect?: () => Promise<unknown>;
};

function isFreighterLike(value: unknown): value is FreighterLike {
  if (!isRecord(value)) return false;
  return (
    typeof (value as FreighterLike).requestAccess === "function" ||
    typeof (value as FreighterLike).getAddress === "function"
  );
}

function injectedFreighter(): FreighterLike | null {
  const win = browserWindow();
  if (!win) return null;
  if (isFreighterLike(win.freighter)) return win.freighter;
  if (isFreighterLike(win.freighterApi)) return win.freighterApi;
  return null;
}

async function loadFreighter(): Promise<FreighterLike | null> {
  const injected = injectedFreighter();
  if (injected) return injected;
  try {
    const freighterModule = await import("@stellar/freighter-api");
    const candidate = ((freighterModule as { default?: unknown }).default ?? freighterModule) as unknown;
    return isFreighterLike(candidate) ? candidate : null;
  } catch {
    return null;
  }
}

function freighterInstalledSync(): boolean {
  return injectedFreighter() !== null;
}

async function freighterConnected(): Promise<boolean> {
  const api = injectedFreighter();
  if (api) return true;
  try {
    const freighterModule = await import("@stellar/freighter-api");
    const candidate = ((freighterModule as { default?: unknown }).default ?? freighterModule) as unknown;
    if (!isFreighterLike(candidate) || typeof candidate.isConnected !== "function") {
      return false;
    }
    const status = await candidate.isConnected();
    if (isRecord(status) && typeof status.isConnected === "boolean") {
      return status.isConnected;
    }
    return Boolean(status);
  } catch {
    return false;
  }
}

const freighterStore: { api: FreighterLike | null; publicKey: string | null } = {
  api: null,
  publicKey: null,
};

const freighterAdapter: WalletAdapter = {
  id: "freighter",
  name: "Freighter",
  installUrl: "https://www.freighter.app/",
  isInstalled: freighterInstalledSync,
  async connect() {
    const api = await loadFreighter();
    if (!api || typeof api.requestAccess !== "function") {
      throw new WalletNotInstalledError("freighter");
    }
    if (!(await freighterConnected())) {
      throw new WalletNotInstalledError("freighter");
    }
    const access = await api.requestAccess();
    const error = extractError(access);
    if (error) throw new Error(error);
    const address = isRecord(access) ? pickString(access, ["address", "publicKey"]) : "";
    if (!address) throw new Error("Freighter connection was rejected");
    freighterStore.api = api;
    freighterStore.publicKey = address;
    return address;
  },
  async disconnect() {
    const api = freighterStore.api ?? injectedFreighter();
    try {
      if (api && typeof api.disconnect === "function") await api.disconnect();
    } catch {
      /* wallet-side session clearing is best effort */
    }
    freighterStore.api = null;
    freighterStore.publicKey = null;
  },
  getPublicKey() {
    return freighterStore.publicKey;
  },
  async signTransaction(xdr, options) {
    const api = freighterStore.api ?? (await loadFreighter());
    if (!api || typeof api.signTransaction !== "function") {
      throw new WalletNotInstalledError("freighter");
    }
    const signOptions: UnknownRecord = {};
    if (options?.networkPassphrase) signOptions.networkPassphrase = options.networkPassphrase;
    const address = options?.address ?? freighterStore.publicKey;
    if (address) signOptions.address = address;
    const result = await api.signTransaction(xdr, signOptions);
    const error = extractError(result);
    if (error) throw new Error(error);
    const signed = isRecord(result)
      ? pickString(result, ["signedTxXdr", "signedTransactionXdr", "signed_tx_xdr", "blob"])
      : "";
    if (!signed) throw new Error("Freighter did not return a signed transaction");
    return signed;
  },
};

type AlbedoCallable = (params: UnknownRecord) => Promise<UnknownRecord>;

type AlbedoMethods = {
  publicKey?: (params?: UnknownRecord) => Promise<UnknownRecord>;
  publickey?: (params?: UnknownRecord) => Promise<UnknownRecord>;
  tx?: (params: UnknownRecord) => Promise<UnknownRecord>;
};

type AlbedoLike = AlbedoCallable | AlbedoMethods;

function injectedAlbedo(): AlbedoLike | null {
  const win = browserWindow();
  if (!win) return null;
  const candidate = win.albedo;
  if (typeof candidate === "function") return candidate as AlbedoCallable;
  if (isRecord(candidate) && (candidate.tx || candidate.publicKey || candidate.publickey)) {
    return candidate as unknown as AlbedoMethods;
  }
  return null;
}

let albedoPublicKey: string | null = null;

const albedoAdapter: WalletAdapter = {
  id: "albedo",
  name: "Albedo",
  installUrl: "https://albedo.link/",
  isInstalled() {
    return injectedAlbedo() !== null;
  },
  async connect() {
    const albedo = injectedAlbedo();
    if (!albedo) throw new WalletNotInstalledError("albedo");
    let pubkey = "";
    if (typeof albedo === "function") {
      const result = await albedo({ intent: "publickey" });
      pubkey = pickString(result, ["pubkey", "publicKey"]);
    } else if (typeof albedo.publicKey === "function") {
      const result = await albedo.publicKey();
      pubkey = pickString(result, ["pubkey", "publicKey"]);
    } else if (typeof albedo.publickey === "function") {
      const result = await albedo.publickey();
      pubkey = pickString(result, ["pubkey", "publicKey"]);
    }
    if (!pubkey) throw new Error("Albedo connection was rejected");
    albedoPublicKey = pubkey;
    return pubkey;
  },
  async disconnect() {
    albedoPublicKey = null;
  },
  getPublicKey() {
    return albedoPublicKey;
  },
  async signTransaction(xdr, options) {
    const albedo = injectedAlbedo();
    if (!albedo) throw new WalletNotInstalledError("albedo");
    const params: UnknownRecord = { xdr };
    if (options?.networkPassphrase) params.network = options.networkPassphrase;
    const pubkey = options?.address ?? albedoPublicKey;
    if (pubkey) params.pubkey = pubkey;
    const result =
      typeof albedo === "function"
        ? await albedo({ intent: "tx", ...params })
        : albedo.tx
          ? await albedo.tx(params)
          : null;
    if (!result) throw new Error("Albedo transaction signing is unavailable");
    const signed = pickString(result, ["signed_envelope_xdr", "signedTxXdr", "signed_envelope"]);
    if (!signed) throw new Error("Albedo did not return a signed transaction");
    return signed;
  },
};

type RabetLike = {
  connect?: () => Promise<UnknownRecord>;
  sign?: (xdr: string, network?: string) => Promise<UnknownRecord>;
  disconnect?: () => Promise<unknown>;
  close?: () => void;
};

function injectedRabet(): RabetLike | null {
  const win = browserWindow();
  if (!win || !isRecord(win.rabet)) return null;
  return win.rabet as RabetLike;
}

let rabetPublicKey: string | null = null;

function rabetNetwork(options?: WalletSignOptions): string {
  if (options?.network === "testnet" || options?.network === "mainnet") return options.network;
  const passphrase = options?.networkPassphrase ?? "";
  return /test/i.test(passphrase) ? "testnet" : "mainnet";
}

const rabetAdapter: WalletAdapter = {
  id: "rabet",
  name: "Rabet",
  installUrl: "https://rabet.io/",
  isInstalled() {
    return injectedRabet() !== null;
  },
  async connect() {
    const rabet = injectedRabet();
    if (!rabet || typeof rabet.connect !== "function") {
      throw new WalletNotInstalledError("rabet");
    }
    const result = await rabet.connect();
    const error = extractError(result);
    if (error) throw new Error(error);
    const publicKey = pickString(result, ["publicKey", "pubkey", "address"]);
    if (!publicKey) throw new Error("Rabet connection was rejected");
    rabetPublicKey = publicKey;
    return publicKey;
  },
  async disconnect() {
    const rabet = injectedRabet();
    try {
      if (rabet && typeof rabet.disconnect === "function") await rabet.disconnect();
    } catch {
      /* best effort */
    }
    rabetPublicKey = null;
  },
  getPublicKey() {
    return rabetPublicKey;
  },
  async signTransaction(xdr, options) {
    const rabet = injectedRabet();
    if (!rabet || typeof rabet.sign !== "function") {
      throw new WalletNotInstalledError("rabet");
    }
    const result = await rabet.sign(xdr, rabetNetwork(options));
    const error = extractError(result);
    if (error) throw new Error(error);
    const signed = pickString(result, ["xdr", "signedTxXdr", "signedTransactionXdr"]);
    if (!signed) throw new Error("Rabet did not return a signed transaction");
    return signed;
  },
};

type XBullLike = {
  connect?: (permissions: { canRequestPublicKey: boolean; canRequestSign: boolean }) => Promise<unknown>;
  getPublicKey?: () => Promise<string>;
  signXDR?: (xdr: string, options?: { network?: string; publicKey?: string }) => Promise<string>;
  disconnect?: () => Promise<unknown>;
};

function injectedXBull(): XBullLike | null {
  const win = browserWindow();
  if (!win || !isRecord(win.xBullSDK)) return null;
  return win.xBullSDK as XBullLike;
}

let xbullPublicKey: string | null = null;

const xbullAdapter: WalletAdapter = {
  id: "xbull",
  name: "xBull",
  installUrl: "https://xbull.app/",
  isInstalled() {
    return injectedXBull() !== null;
  },
  async connect() {
    const sdk = injectedXBull();
    if (!sdk || typeof sdk.connect !== "function" || typeof sdk.getPublicKey !== "function") {
      throw new WalletNotInstalledError("xbull");
    }
    await sdk.connect({ canRequestPublicKey: true, canRequestSign: true });
    const publicKey = await sdk.getPublicKey();
    if (!publicKey) throw new Error("xBull connection was rejected");
    xbullPublicKey = publicKey;
    return publicKey;
  },
  async disconnect() {
    const sdk = injectedXBull();
    try {
      if (sdk && typeof sdk.disconnect === "function") await sdk.disconnect();
    } catch {
      /* best effort */
    }
    xbullPublicKey = null;
  },
  getPublicKey() {
    return xbullPublicKey;
  },
  async signTransaction(xdr, options) {
    const sdk = injectedXBull();
    if (!sdk || typeof sdk.signXDR !== "function") {
      throw new WalletNotInstalledError("xbull");
    }
    const signOptions: { network?: string; publicKey?: string } = {};
    if (options?.networkPassphrase) signOptions.network = options.networkPassphrase;
    const publicKey = options?.address ?? xbullPublicKey;
    if (publicKey) signOptions.publicKey = publicKey;
    const signed =
      signOptions.network && signOptions.publicKey
        ? await sdk.signXDR(xdr, signOptions)
        : await sdk.signXDR(xdr);
    if (!signed) throw new Error("xBull did not return a signed transaction");
    return signed;
  },
};

const LOBSTR_REQUEST_SOURCE = "LOBSTR_EXTERNAL_MSG_REQUEST";
const LOBSTR_RESPONSE_SOURCE = "LOBSTR_EXTERNAL_MSG_RESPONSE";
const LOBSTR_CONNECTION_STORAGE_KEY = "LOBSTR_CONNECTION_KEY";
const LOBSTR_STATUS_TIMEOUT_MS = 2000;

type LobstrResponse = UnknownRecord & {
  publicKey?: string;
  connectionKey?: string;
  signedData?: string;
  signerAddress?: string;
  isConnected?: boolean;
  error?: string;
};

function lobstrConnectionKey(): string {
  const win = browserWindow();
  if (!win || !win.sessionStorage) return "";
  try {
    return win.sessionStorage.getItem(LOBSTR_CONNECTION_STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

function lobstrStoreConnectionKey(key: string): void {
  const win = browserWindow();
  if (!win || !win.sessionStorage) return;
  try {
    if (key) win.sessionStorage.setItem(LOBSTR_CONNECTION_STORAGE_KEY, key);
    else win.sessionStorage.removeItem(LOBSTR_CONNECTION_STORAGE_KEY);
  } catch {
    /* storage may be unavailable in private modes */
  }
}

function lobstrSend(payload: UnknownRecord, timeoutMs?: number): Promise<LobstrResponse> {
  const win = browserWindow();
  if (!win) return Promise.reject(new Error("LOBSTR signer requires a browser environment"));
  const messageId = Date.now() + Math.random();

  return new Promise((resolve, reject) => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const cleanup = () => {
      win.removeEventListener("message", handler);
      if (timer !== undefined) clearTimeout(timer);
    };
    const handler = (event: MessageEvent) => {
      const data = event.data as LobstrResponse | null;
      if (event.source !== window) return;
      if (!isRecord(data) || data.source !== LOBSTR_RESPONSE_SOURCE) return;
      const echoedId = (data as UnknownRecord).messagedId ?? (data as UnknownRecord).messageId;
      if (echoedId !== messageId) return;
      cleanup();
      resolve(data);
    };
    if (timeoutMs !== undefined) {
      timer = setTimeout(() => {
        cleanup();
        reject(new Error("The LOBSTR signer did not respond"));
      }, timeoutMs);
    }
    win.addEventListener("message", handler, false);
    win.postMessage(
      { source: LOBSTR_REQUEST_SOURCE, messageId, ...payload },
      win.location.origin
    );
  });
}

function lobstrInstalled(): boolean {
  const win = browserWindow();
  if (!win) return false;
  if (win.lobstrSignerExtension) return true;
  return false;
}

let lobstrPublicKey: string | null = null;

const lobstrAdapter: WalletAdapter = {
  id: "lobstr",
  name: "LOBSTR",
  installUrl: "https://lobstr.co/",
  isInstalled: lobstrInstalled,
  async connect() {
    if (!lobstrInstalled()) throw new WalletNotInstalledError("lobstr");
    const response = await lobstrSend({ type: "REQUEST_ACCESS", version: 1 });
    if (response.error) throw new Error(response.error);
    const publicKey = response.publicKey ?? "";
    if (!publicKey) throw new Error("LOBSTR connection was rejected");
    lobstrStoreConnectionKey(response.connectionKey ?? "");
    lobstrPublicKey = publicKey;
    return publicKey;
  },
  async disconnect() {
    lobstrStoreConnectionKey("");
    lobstrPublicKey = null;
  },
  getPublicKey() {
    return lobstrPublicKey;
  },
  async signTransaction(xdr) {
    if (!lobstrInstalled()) throw new WalletNotInstalledError("lobstr");
    const response = await lobstrSend({
      type: "SIGN",
      version: 1,
      dataToSign: xdr,
      signType: "transaction",
      connectionKey: lobstrConnectionKey(),
    });
    if (response.error) throw new Error(response.error);
    const signed = response.signedData ?? "";
    if (!signed) throw new Error("LOBSTR did not return a signed transaction");
    if (response.signerAddress) lobstrPublicKey = response.signerAddress;
    return signed;
  },
};

const adapters: Record<WalletId, WalletAdapter> = {
  freighter: freighterAdapter,
  albedo: albedoAdapter,
  rabet: rabetAdapter,
  xbull: xbullAdapter,
  lobstr: lobstrAdapter,
};

export function getWalletAdapter(id: WalletId): WalletAdapter {
  const adapter = adapters[id];
  if (!adapter) throw new WalletNotInstalledError(null, `Unknown wallet id: ${String(id)}`);
  return adapter;
}

export function detectInstalledWallets(): WalletId[] {
  return WALLET_OPTIONS.filter((option) => adapters[option.id].isInstalled()).map(
    (option) => option.id
  );
}

export async function connectWallet(
  id?: WalletId
): Promise<{ id: WalletId; address: string }> {
  const installed = detectInstalledWallets();
  const target = id ?? installed[0];
  if (!target) {
    showWalletInstallModal();
    throw new WalletNotInstalledError(null, "No supported Stellar wallet was detected");
  }
  const adapter = getWalletAdapter(target);
  if (!adapter.isInstalled()) {
    showWalletInstallModal(target);
    throw new WalletNotInstalledError(target);
  }
  try {
    const address = await adapter.connect();
    return { id: target, address };
  } catch (error) {
    if (error instanceof WalletNotInstalledError) {
      showWalletInstallModal(target);
    }
    throw error;
  }
}

const MODAL_ROOT_ID = "tessera-wallet-install-modal";

let modalTeardown: (() => void) | null = null;

export function hideWalletInstallModal(): void {
  if (!modalTeardown) return;
  modalTeardown();
  modalTeardown = null;
}

export function showWalletInstallModal(focusWalletId?: WalletId): void {
  if (typeof document === "undefined") return;
  hideWalletInstallModal();

  const previouslyFocused = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const detected = new Set(detectInstalledWallets());

  const overlay = document.createElement("div");
  overlay.id = MODAL_ROOT_ID;
  overlay.setAttribute("data-testid", "wallet-install-modal");
  Object.assign(overlay.style, {
    position: "fixed",
    inset: "0",
    zIndex: "9999",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    background: "rgba(0, 0, 0, 0.65)",
    backdropFilter: "blur(2px)",
  });

  const dialog = document.createElement("div");
  dialog.setAttribute("role", "dialog");
  dialog.setAttribute("aria-modal", "true");
  dialog.setAttribute("aria-labelledby", "wallet-install-modal-title");
  dialog.tabIndex = -1;
  Object.assign(dialog.style, {
    width: "calc(100% - 32px)",
    maxWidth: "420px",
    background: "#0b1220",
    color: "#e5e7eb",
    border: "1px solid rgba(255, 255, 255, 0.14)",
    borderRadius: "14px",
    padding: "20px",
    boxShadow: "0 18px 48px rgba(0, 0, 0, 0.45)",
    fontFamily: "ui-sans-serif, system-ui, sans-serif",
  });

  const title = document.createElement("h2");
  title.id = "wallet-install-modal-title";
  title.textContent = "Connect a Stellar wallet";
  Object.assign(title.style, { margin: "0 0 4px", fontSize: "16px", fontWeight: "600" });

  const subtitle = document.createElement("p");
  const focusMeta = focusWalletId
    ? WALLET_OPTIONS.find((option) => option.id === focusWalletId)
    : undefined;
  subtitle.textContent = focusMeta && !detected.has(focusMeta.id)
    ? `${focusMeta.name} was not detected in this browser. Install it to continue, or pick another wallet below.`
    : detected.size === 0
      ? "No supported wallet was detected. Install one of the wallets below to continue."
      : "Choose a wallet to continue.";
  Object.assign(subtitle.style, {
    margin: "0 0 14px",
    fontSize: "13px",
    lineHeight: "1.5",
    color: "rgba(229, 231, 235, 0.7)",
  });

  const list = document.createElement("div");
  Object.assign(list.style, {
    display: "flex",
    flexDirection: "column",
    gap: "8px",
    margin: "0 0 16px",
  });

  for (const option of WALLET_OPTIONS) {
    const row = document.createElement("div");
    Object.assign(row.style, {
      display: "flex",
      alignItems: "center",
      justifyContent: "space-between",
      gap: "12px",
      padding: "8px 10px",
      borderRadius: "10px",
      background:
        focusWalletId === option.id ? "rgba(16, 185, 129, 0.12)" : "rgba(255, 255, 255, 0.04)",
      border: "1px solid rgba(255, 255, 255, 0.07)",
    });

    const label = document.createElement("span");
    label.textContent = option.name;
    Object.assign(label.style, { fontSize: "13px", fontWeight: "600" });
    row.appendChild(label);

    if (detected.has(option.id)) {
      const badge = document.createElement("span");
      badge.textContent = "Detected";
      badge.setAttribute("data-testid", `wallet-detected-${option.id}`);
      Object.assign(badge.style, {
        fontSize: "11px",
        color: "#6ee7b7",
        fontWeight: "600",
      });
      row.appendChild(badge);
    } else {
      const link = document.createElement("a");
      link.href = option.installUrl;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      link.textContent = "Install →";
      link.setAttribute("data-testid", `wallet-install-link-${option.id}`);
      Object.assign(link.style, {
        fontSize: "12px",
        fontWeight: "600",
        color: "#34d399",
        textDecoration: "none",
      });
      row.appendChild(link);
    }

    list.appendChild(row);
  }

  const closeButton = document.createElement("button");
  closeButton.type = "button";
  closeButton.textContent = "Close";
  closeButton.setAttribute("data-testid", "wallet-install-modal-close");
  Object.assign(closeButton.style, {
    width: "100%",
    padding: "8px 12px",
    borderRadius: "10px",
    border: "1px solid rgba(255, 255, 255, 0.16)",
    background: "rgba(255, 255, 255, 0.08)",
    color: "#e5e7eb",
    fontSize: "13px",
    fontWeight: "600",
    cursor: "pointer",
  });

  dialog.append(title, subtitle, list, closeButton);
  overlay.appendChild(dialog);

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") hideWalletInstallModal();
  };
  const onOverlayClick = (event: MouseEvent) => {
    if (event.target === overlay) hideWalletInstallModal();
  };

  closeButton.addEventListener("click", hideWalletInstallModal);
  document.addEventListener("keydown", onKeyDown);
  overlay.addEventListener("click", onOverlayClick);
  document.body.appendChild(overlay);
  dialog.focus();

  modalTeardown = () => {
    closeButton.removeEventListener("click", hideWalletInstallModal);
    document.removeEventListener("keydown", onKeyDown);
    overlay.removeEventListener("click", onOverlayClick);
    overlay.remove();
    if (previouslyFocused && document.contains(previouslyFocused)) previouslyFocused.focus();
  };
}
