"use client";

import { useCallback, useEffect, useId, useState } from "react";
import {
  isConnected,
  isAllowed,
  requestAccess,
  getAddress,
  getNetworkDetails,
} from "@stellar/freighter-api";

// ── Types ────────────────────────────────────────────────────────────────────

type BridgeStep = 1 | 2 | 3;

/** Visual state for each step in the status tracker. */
type StepStatus = "pending" | "active" | "done" | "timeout";

type StellarWalletState =
  | "checking"
  | "unavailable"
  | "disconnected"
  | "connecting"
  | "connected"
  | "error";

type EvmWalletState =
  | "checking"
  | "unavailable"
  | "disconnected"
  | "connecting"
  | "connected"
  | "error";

type BridgeRunState = "idle" | "running" | "done" | "timeout" | "refunding" | "refunded";

interface BridgeProgress {
  step: BridgeStep;
  stepStatuses: Record<BridgeStep, StepStatus>;
  bridgeState: BridgeRunState;
  errorMessage: string | null;
}

// ── Constants ────────────────────────────────────────────────────────────────

/** How long to wait for each simulated bridge step before treating as timeout (ms). */
const STEP_TIMEOUT_MS = 20_000;

/** How long to wait for a refund to process (ms). */
const REFUND_DELAY_MS = 2_000;

const STEP_LABELS: Record<BridgeStep, string> = {
  1: "Lock on Source",
  2: "Validator Verification",
  3: "Mint on Destination",
};

const STEP_DESCRIPTIONS: Record<BridgeStep, string> = {
  1: "Locking your tokens in the Stellar bridge contract. Freighter will prompt you to sign.",
  2: "Validators are verifying the lock event and reaching consensus. This usually takes 10–30 seconds.",
  3: "Minting wrapped tokens on the destination EVM chain. Confirm in MetaMask / Rabby.",
};

const INITIAL_STEP_STATUSES: Record<BridgeStep, StepStatus> = {
  1: "pending",
  2: "pending",
  3: "pending",
};

// ── Helpers ──────────────────────────────────────────────────────────────────

function shortenAddress(address: string): string {
  if (address.length <= 12) return address;
  return `${address.slice(0, 6)}…${address.slice(-4)}`;
}

function isValidStellarAddress(value: string): boolean {
  return /^G[A-Z2-7]{55}$/.test(value.trim());
}

function isValidEvmAddress(value: string): boolean {
  return /^0x[0-9a-fA-F]{40}$/.test(value.trim());
}

function isPositiveNumber(value: string): boolean {
  const n = Number(value);
  return Number.isFinite(n) && n > 0;
}

// ── Stellar wallet hook ───────────────────────────────────────────────────────

function useStellarWallet() {
  const [state, setState] = useState<StellarWalletState>("checking");
  const [address, setAddress] = useState<string | null>(null);
  const [network, setNetwork] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const connected = await isConnected();
        if (cancelled) return;
        if (!connected.isConnected) {
          setState("unavailable");
          return;
        }
        const allowed = await isAllowed();
        if (cancelled) return;
        if (allowed.isAllowed) {
          const addr = await getAddress();
          const net = await getNetworkDetails();
          if (cancelled) return;
          if (!addr.error && !net.error) {
            setAddress(addr.address);
            setNetwork(net.network);
            setState("connected");
            return;
          }
        }
        setState("disconnected");
      } catch {
        if (!cancelled) setState("unavailable");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const connect = useCallback(async () => {
    setState("connecting");
    setError(null);
    try {
      const access = await requestAccess();
      if (access.error) throw new Error(access.error.message);
      const addr = await getAddress();
      if (addr.error) throw new Error(addr.error.message);
      const net = await getNetworkDetails();
      if (net.error) throw new Error(net.error.message);
      setAddress(addr.address);
      setNetwork(net.network);
      setState("connected");
    } catch (e) {
      setError(e instanceof Error ? e.message : "Freighter connection failed.");
      setState("error");
    }
  }, []);

  return { state, address, network, error, connect };
}

// ── EVM wallet hook ───────────────────────────────────────────────────────────

function useEvmWallet() {
  const [state, setState] = useState<EvmWalletState>("checking");
  const [address, setAddress] = useState<string | null>(null);
  const [chainId, setChainId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (typeof window === "undefined") {
      setState("unavailable");
      return;
    }
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const ethereum = (window as any).ethereum;
    if (!ethereum) {
      setState("unavailable");
      return;
    }
    setState("disconnected");
  }, []);

  const connect = useCallback(async () => {
    setState("connecting");
    setError(null);
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const ethereum = (window as any).ethereum;
      if (!ethereum) throw new Error("No EVM wallet detected. Install MetaMask or Rabby.");
      const accounts: string[] = await ethereum.request({ method: "eth_requestAccounts" });
      if (!accounts || accounts.length === 0) throw new Error("No accounts returned from wallet.");
      const chain: string = await ethereum.request({ method: "eth_chainId" });
      setAddress(accounts[0]);
      setChainId(chain);
      setState("connected");
    } catch (e) {
      setError(e instanceof Error ? e.message : "EVM wallet connection failed.");
      setState("error");
    }
  }, []);

  return { state, address, chainId, error, connect };
}

// ── Bridge progress simulation ───────────────────────────────────────────────

/**
 * Simulates the three-step bridge flow.  In production this would drive real
 * contract calls; here it advances through steps with configurable delays so
 * the timeout / refund paths are exercisable in a docs context.
 */
function useBridgeFlow() {
  const [progress, setProgress] = useState<BridgeProgress>({
    step: 1,
    stepStatuses: { ...INITIAL_STEP_STATUSES },
    bridgeState: "idle",
    errorMessage: null,
  });

  const reset = useCallback(() => {
    setProgress({
      step: 1,
      stepStatuses: { ...INITIAL_STEP_STATUSES },
      bridgeState: "idle",
      errorMessage: null,
    });
  }, []);

  const start = useCallback(() => {
    setProgress({
      step: 1,
      stepStatuses: { 1: "active", 2: "pending", 3: "pending" },
      bridgeState: "running",
      errorMessage: null,
    });

    let currentStep: BridgeStep = 1;
    let timeoutId: ReturnType<typeof setTimeout>;
    let didTimeout = false;

    function advanceToNextStep() {
      const next = (currentStep + 1) as BridgeStep;
      if (next > 3) {
        // All steps complete
        setProgress({
          step: 3,
          stepStatuses: { 1: "done", 2: "done", 3: "done" },
          bridgeState: "done",
          errorMessage: null,
        });
        return;
      }
      currentStep = next;
      setProgress((prev) => ({
        ...prev,
        step: next,
        stepStatuses: {
          ...prev.stepStatuses,
          [(next - 1) as BridgeStep]: "done",
          [next]: "active",
        },
      }));
      scheduleNext();
    }

    function scheduleNext() {
      // Each step takes a varied simulated delay: step 1 = 2 s, step 2 = 4 s, step 3 = 2 s
      const delays: Record<BridgeStep, number> = { 1: 2000, 2: 4000, 3: 2000 };
      timeoutId = setTimeout(advanceToNextStep, delays[currentStep]);
    }

    // Arm the overall bridge timeout
    const overallTimeout = setTimeout(() => {
      didTimeout = true;
      clearTimeout(timeoutId);
      setProgress((prev) => ({
        ...prev,
        bridgeState: "timeout",
        stepStatuses: {
          ...prev.stepStatuses,
          [prev.step]: "timeout",
        },
        errorMessage:
          "The bridge transaction timed out waiting for validator confirmation. A refund has been triggered automatically.",
      }));
    }, STEP_TIMEOUT_MS);

    scheduleNext();

    // Return cleanup (not directly used but keeps the pattern consistent)
    return () => {
      didTimeout = true;
      clearTimeout(timeoutId);
      clearTimeout(overallTimeout);
    };
  }, []);

  const triggerRefund = useCallback(() => {
    setProgress((prev) => ({
      ...prev,
      bridgeState: "refunding",
      errorMessage: null,
    }));
    setTimeout(() => {
      setProgress((prev) => ({
        ...prev,
        bridgeState: "refunded",
        errorMessage: null,
      }));
    }, REFUND_DELAY_MS);
  }, []);

  return { progress, start, reset, triggerRefund };
}

// ── Sub-components ────────────────────────────────────────────────────────────

interface WalletPanelProps {
  label: string;
  walletName: string;
  state: StellarWalletState | EvmWalletState;
  address: string | null;
  extra?: string | null;
  error?: string | null;
  onConnect: () => void;
  disabled?: boolean;
}

function WalletPanel({
  label,
  walletName,
  state,
  address,
  extra,
  error,
  onConnect,
  disabled = false,
}: WalletPanelProps) {
  const isConnectedState = state === "connected";
  const isConnecting = state === "connecting";

  return (
    <div
      className={`rounded-xl border p-4 transition ${
        isConnectedState
          ? "border-emerald-500/30 bg-emerald-500/5"
          : "border-white/10 bg-white/[0.03]"
      }`}
    >
      <div className="mb-3 flex items-center justify-between gap-2">
        <div>
          <p className="text-xs font-semibold uppercase tracking-widest text-brand-300">{label}</p>
          <p className="mt-0.5 text-sm font-medium text-white">{walletName}</p>
        </div>
        <span
          aria-hidden="true"
          className={`h-2.5 w-2.5 rounded-full ${
            isConnectedState
              ? "bg-emerald-400"
              : state === "error"
                ? "bg-red-400"
                : "bg-base-500"
          }`}
        />
      </div>

      {isConnectedState && address ? (
        <div className="space-y-1">
          <p className="break-all font-mono text-xs text-base-200" aria-label={`${label} address`}>
            {shortenAddress(address)}
          </p>
          {extra && <p className="text-xs text-base-400">{extra}</p>}
        </div>
      ) : (
        <button
          type="button"
          onClick={onConnect}
          disabled={disabled || isConnecting || state === "unavailable"}
          className="w-full rounded-lg border border-brand-500/30 bg-brand-500/10 py-2 text-sm font-semibold text-brand-100 transition hover:bg-brand-500/20 disabled:cursor-not-allowed disabled:opacity-50"
          aria-label={`Connect ${walletName}`}
        >
          {isConnecting
            ? "Connecting…"
            : state === "unavailable"
              ? `${walletName} not detected`
              : `Connect ${walletName}`}
        </button>
      )}

      {error && (
        <p role="alert" className="mt-2 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-1.5 text-xs text-red-300">
          {error}
        </p>
      )}
    </div>
  );
}

interface StepTrackerProps {
  stepStatuses: Record<BridgeStep, StepStatus>;
  currentStep: BridgeStep;
}

function StepTracker({ stepStatuses, currentStep }: StepTrackerProps) {
  const steps: BridgeStep[] = [1, 2, 3];

  return (
    <ol aria-label="Bridge transaction steps" className="space-y-3">
      {steps.map((step) => {
        const status = stepStatuses[step];
        const isActive = step === currentStep && status === "active";
        const isDone = status === "done";
        const isTimeout = status === "timeout";

        return (
          <li
            key={step}
            aria-current={isActive ? "step" : undefined}
            className={`flex items-start gap-3 rounded-xl border px-4 py-3 transition ${
              isDone
                ? "border-emerald-500/25 bg-emerald-500/5"
                : isActive
                  ? "border-brand-500/30 bg-brand-500/5"
                  : isTimeout
                    ? "border-red-500/30 bg-red-500/5"
                    : "border-white/8 bg-white/[0.02]"
            }`}
          >
            {/* Step indicator */}
            <span
              aria-hidden="true"
              className={`mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-bold ${
                isDone
                  ? "bg-emerald-500 text-white"
                  : isActive
                    ? "bg-brand-500 text-base-950"
                    : isTimeout
                      ? "bg-red-500 text-white"
                      : "bg-white/10 text-base-400"
              }`}
            >
              {isDone ? "✓" : isTimeout ? "!" : step}
            </span>

            <div className="min-w-0 flex-1">
              <p
                className={`text-sm font-semibold ${
                  isDone
                    ? "text-emerald-200"
                    : isActive
                      ? "text-white"
                      : isTimeout
                        ? "text-red-300"
                        : "text-base-400"
                }`}
              >
                {STEP_LABELS[step]}
              </p>
              <p className="mt-0.5 text-xs text-base-400">{STEP_DESCRIPTIONS[step]}</p>
            </div>

            {isActive && (
              <span
                aria-label="In progress"
                className="mt-1.5 h-2 w-2 shrink-0 animate-pulse rounded-full bg-brand-400"
              />
            )}
          </li>
        );
      })}
    </ol>
  );
}

// ── Main component ────────────────────────────────────────────────────────────

export function BridgeAssistant() {
  const titleId = useId();

  // Wallets
  const stellar = useStellarWallet();
  const evm = useEvmWallet();

  // Form
  const [amount, setAmount] = useState("");
  const [destinationAddress, setDestinationAddress] = useState("");
  const [formErrors, setFormErrors] = useState<Record<string, string>>({});

  // Bridge flow
  const { progress, start, reset, triggerRefund } = useBridgeFlow();

  const bothConnected = stellar.state === "connected" && evm.state === "connected";
  const isIdle = progress.bridgeState === "idle";
  const isRunning = progress.bridgeState === "running";
  const isDone = progress.bridgeState === "done";
  const isTimeout = progress.bridgeState === "timeout";
  const isRefunding = progress.bridgeState === "refunding";
  const isRefunded = progress.bridgeState === "refunded";

  function validate(): boolean {
    const errors: Record<string, string> = {};
    if (!isPositiveNumber(amount)) errors.amount = "Enter a positive amount.";
    if (!isValidEvmAddress(destinationAddress)) {
      errors.destinationAddress = "Enter a valid EVM address (0x…, 40 hex chars).";
    }
    if (stellar.state !== "connected") errors.stellar = "Connect Freighter first.";
    if (evm.state !== "connected") errors.evm = "Connect MetaMask / Rabby first.";
    setFormErrors(errors);
    return Object.keys(errors).length === 0;
  }

  function handleStart() {
    if (!validate()) return;
    start();
  }

  function handleReset() {
    reset();
    setAmount("");
    setDestinationAddress("");
    setFormErrors({});
  }

  // Auto-trigger refund when timeout fires
  useEffect(() => {
    if (isTimeout) {
      const id = setTimeout(triggerRefund, 3000);
      return () => clearTimeout(id);
    }
  }, [isTimeout, triggerRefund]);

  return (
    <section
      className="my-12 rounded-2xl border border-white/10 bg-base-900/70 p-6 shadow-2xl shadow-brand-500/5"
      aria-labelledby={titleId}
    >
      {/* Header */}
      <div className="mb-6 flex flex-col gap-2 md:flex-row md:items-center md:justify-between">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.2em] text-brand-300">
            Cross-chain bridge
          </p>
          <h2 id={titleId} className="mt-1.5 text-2xl font-semibold text-white">
            Token Migration Assistant
          </h2>
          <p className="mt-1 text-sm text-base-400">
            Migrate tokens from Stellar Soroban to an EVM destination chain via the bridge contract.
          </p>
        </div>
        <span className="inline-flex items-center rounded-full border border-gold-500/30 bg-gold-500/10 px-3 py-1 text-xs font-semibold text-gold-300">
          Testnet only
        </span>
      </div>

      {/* Dual wallet connection */}
      <div className="mb-6 grid gap-4 md:grid-cols-2">
        <WalletPanel
          label="Source chain"
          walletName="Freighter (Stellar)"
          state={stellar.state}
          address={stellar.address}
          extra={stellar.network ? `Network: ${stellar.network}` : null}
          error={stellar.error ?? formErrors.stellar}
          onConnect={stellar.connect}
          disabled={isRunning}
        />
        <WalletPanel
          label="Destination chain"
          walletName="MetaMask / Rabby (EVM)"
          state={evm.state}
          address={evm.address}
          extra={evm.chainId ? `Chain ID: ${parseInt(evm.chainId, 16)}` : null}
          error={evm.error ?? formErrors.evm}
          onConnect={evm.connect}
          disabled={isRunning}
        />
      </div>

      {/* Transfer form — visible while idle */}
      {isIdle && (
        <div className="mb-6 grid gap-4 md:grid-cols-2">
          <label className="flex flex-col gap-2 text-sm text-base-100">
            Amount to bridge
            <input
              type="number"
              min="0"
              step="any"
              value={amount}
              onChange={(e) => {
                setAmount(e.target.value);
                setFormErrors((prev) => ({ ...prev, amount: "" }));
              }}
              disabled={isRunning}
              placeholder="e.g. 100"
              aria-label="Amount to bridge"
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500 disabled:opacity-50"
            />
            {formErrors.amount && (
              <span role="alert" className="text-xs text-red-300">
                {formErrors.amount}
              </span>
            )}
          </label>

          <label className="flex flex-col gap-2 text-sm text-base-100">
            Destination EVM address
            <input
              type="text"
              value={destinationAddress}
              onChange={(e) => {
                setDestinationAddress(e.target.value);
                setFormErrors((prev) => ({ ...prev, destinationAddress: "" }));
              }}
              disabled={isRunning}
              placeholder="0x…"
              aria-label="Destination EVM address"
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 font-mono text-white outline-none transition focus:border-brand-500 disabled:opacity-50"
            />
            {formErrors.destinationAddress && (
              <span role="alert" className="text-xs text-red-300">
                {formErrors.destinationAddress}
              </span>
            )}
          </label>
        </div>
      )}

      {/* Step tracker — visible while running or after */}
      {!isIdle && (
        <div className="mb-6">
          <h3 className="mb-3 text-sm font-semibold uppercase tracking-widest text-base-300">
            Transaction status
          </h3>
          <StepTracker
            stepStatuses={progress.stepStatuses}
            currentStep={progress.step}
          />
        </div>
      )}

      {/* Status messages */}
      {isDone && (
        <div
          role="status"
          className="mb-5 rounded-xl border border-emerald-500/30 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-200"
        >
          <span className="font-semibold">Bridge complete.</span> Your tokens have been minted on the
          destination chain. Check your EVM wallet at {shortenAddress(destinationAddress || "0x…")}.
        </div>
      )}

      {isTimeout && (
        <div
          role="alert"
          className="mb-5 rounded-xl border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-200"
        >
          <span className="font-semibold">Timeout detected.</span>{" "}
          {progress.errorMessage} Initiating automatic refund in 3 seconds…
        </div>
      )}

      {isRefunding && (
        <div
          role="status"
          className="mb-5 rounded-xl border border-brand-500/30 bg-brand-500/10 px-4 py-3 text-sm text-brand-200"
        >
          <span className="font-semibold">Refund in progress.</span> Unlocking tokens on the source
          chain. This may take a few seconds.
        </div>
      )}

      {isRefunded && (
        <div
          role="status"
          className="mb-5 rounded-xl border border-emerald-500/30 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-200"
        >
          <span className="font-semibold">Refund complete.</span> Your tokens have been returned to
          your Stellar wallet. No funds were lost.
        </div>
      )}

      {/* Actions */}
      <div className="flex flex-wrap items-center gap-3 border-t border-white/10 pt-5">
        {isIdle && (
          <button
            type="button"
            onClick={handleStart}
            disabled={!bothConnected}
            className="rounded-xl bg-brand-500 px-5 py-2.5 text-sm font-semibold text-base-950 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:opacity-50"
          >
            Start bridge transfer
          </button>
        )}

        {isRunning && (
          <button
            type="button"
            disabled
            className="inline-flex items-center gap-2 rounded-xl bg-brand-500/50 px-5 py-2.5 text-sm font-semibold text-base-950 opacity-70"
          >
            <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-base-950 border-t-transparent" />
            Bridge in progress…
          </button>
        )}

        {(isDone || isRefunded) && (
          <button
            type="button"
            onClick={handleReset}
            className="rounded-xl border border-white/10 bg-white/5 px-5 py-2.5 text-sm font-semibold text-white transition hover:bg-white/10"
          >
            Start another transfer
          </button>
        )}

        {isTimeout && (
          <button
            type="button"
            onClick={triggerRefund}
            className="rounded-xl bg-amber-500 px-5 py-2.5 text-sm font-semibold text-base-950 transition hover:bg-amber-400"
          >
            Trigger refund now
          </button>
        )}

        {isRefunding && (
          <button
            type="button"
            disabled
            className="inline-flex items-center gap-2 rounded-xl bg-brand-500/50 px-5 py-2.5 text-sm font-semibold text-base-950 opacity-70"
          >
            <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-base-950 border-t-transparent" />
            Refunding…
          </button>
        )}

        {!bothConnected && isIdle && (
          <p className="text-xs text-base-400">
            Connect both wallets above to enable the transfer.
          </p>
        )}
      </div>
    </section>
  );
}

export default BridgeAssistant;
