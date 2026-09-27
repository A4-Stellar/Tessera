"use client";

import { zodResolver } from "@hookform/resolvers/zod";
import { StrKey } from "@stellar/stellar-sdk";
import { useEffect, useMemo, useState } from "react";
import { useForm } from "react-hook-form";
import { z } from "zod";

const STORAGE_KEY = "tessera_issuer_wizard_state";

function isValidStellarPublicKey(value: string): boolean {
  if (!value.trim()) return false;
  try {
    return StrKey.isValidEd25519PublicKey(value) || StrKey.isValidMed25519PublicKey(value);
  } catch {
    return false;
  }
}

const wizardSchema = z.object({
  assetName: z.string().trim().min(1, "Asset name is required").max(120),
  ticker: z
    .string()
    .trim()
    .min(1, "Ticker symbol is required")
    .max(12)
    .regex(/^[A-Z0-9]+$/i, "Ticker must contain only letters and numbers"),
  issuerPublicKey: z
    .string()
    .trim()
    .min(1, "Issuer public key is required")
    .refine(isValidStellarPublicKey, "Issuer public key must be a valid Stellar public key"),
  totalSupply: z
    .string()
    .trim()
    .min(1, "Total supply is required")
    .refine((value) => Number.isFinite(Number(value)) && Number(value) > 0, "Total supply must be greater than zero"),
  complianceOfficerKey: z
    .string()
    .trim()
    .min(1, "Compliance officer key is required")
    .refine(isValidStellarPublicKey, "Compliance officer key must be a valid Stellar public key"),
  jurisdiction: z.string().trim().min(2, "Jurisdiction is required").max(64),
  allowlistSeed: z
    .string()
    .trim()
    .min(1, "Allowlist seed is required")
    .refine(isValidStellarPublicKey, "Allowlist seed must be a valid Stellar public key"),
});

type WizardValues = z.infer<typeof wizardSchema>;

const EMPTY_VALUES: WizardValues = {
  assetName: "",
  ticker: "",
  issuerPublicKey: "",
  totalSupply: "",
  complianceOfficerKey: "",
  jurisdiction: "US",
  allowlistSeed: "",
};

const steps = [
  { id: "token-setup", title: "Token setup", description: "Define the asset metadata and issuer wallet." },
  { id: "compliance", title: "Compliance configuration", description: "Set the officer key, jurisdiction, and allowlist seed." },
  { id: "deploy", title: "Deployment", description: "Review script output and deploy to wallet." },
];

function readStoredValues(): WizardValues {
  if (typeof window === "undefined") {
    return EMPTY_VALUES;
  }

  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return EMPTY_VALUES;
    const parsed = JSON.parse(raw) as Partial<WizardValues>;
    return { ...EMPTY_VALUES, ...parsed };
  } catch {
    return EMPTY_VALUES;
  }
}

function loadDeploymentScript(values: WizardValues) {
  const script = `#!/usr/bin/env bash
set -e

ASSET_NAME="${values.assetName || "Issuer Asset"}"
TICKER="${values.ticker || "ASSET"}"
ISSUER="${values.issuerPublicKey || "G..."}"
COMPLIANCE_OFFICER="${values.complianceOfficerKey || "G..."}"
JURISDICTION="${values.jurisdiction || "US"}"

stellar contract deploy --network testnet --source "$STELLAR_SECRET_KEY" --wasm ./artifacts/asset_token.wasm
stellar contract invoke --network testnet --source "$STELLAR_SECRET_KEY" \\
  --id "$ASSET_ID" \\
  -- \\
  set_metadata "$ASSET_NAME" "$TICKER" "$ISSUER" "$COMPLIANCE_OFFICER" "$JURISDICTION"

stellar contract invoke --network testnet --source "$STELLAR_SECRET_KEY" \\
  --id "$ASSET_ID" \\
  -- \\
  set_allowlist "${values.allowlistSeed || "G..."}"
`;

  return script;
}

export function IssuerWizard() {
  const [isStarted, setIsStarted] = useState(false);
  const [stepIndex, setStepIndex] = useState(0);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "error">("idle");
  const [deploymentStatus, setDeploymentStatus] = useState<string | null>(null);

  const form = useForm<WizardValues>({
    resolver: zodResolver(wizardSchema),
    defaultValues: readStoredValues(),
    mode: "onChange",
  });

  const values = form.watch();

  useEffect(() => {
    if (!isStarted) return;
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(values));
  }, [isStarted, values]);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const stored = readStoredValues();
    if (stored.assetName || stored.ticker || stored.issuerPublicKey || stored.complianceOfficerKey) {
      form.reset(stored);
      setIsStarted(true);
    }
  }, [form]);

  const currentStep = steps[stepIndex];
  const stepFields = useMemo(() => {
    if (stepIndex === 0) {
      return ["assetName", "ticker", "issuerPublicKey", "totalSupply"] as const;
    }
    if (stepIndex === 1) {
      return ["complianceOfficerKey", "jurisdiction", "allowlistSeed"] as const;
    }
    return [] as const;
  }, [stepIndex]);

  const deploymentScript = useMemo(() => loadDeploymentScript(values), [values]);

  async function nextStep() {
    if (stepIndex >= steps.length - 1) {
      setStepIndex(steps.length - 1);
      return;
    }

    const result = await form.trigger(stepFields);
    if (!result) {
      const firstError = stepFields.find((field) => form.formState.errors[field]);
      if (firstError) {
        form.setFocus(firstError);
      }
      return;
    }
    setStepIndex((current) => Math.min(current + 1, steps.length - 1));
  }

  async function previousStep() {
    setStepIndex((current) => Math.max(current - 1, 0));
  }

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(deploymentScript);
      setCopyState("copied");
      window.setTimeout(() => setCopyState("idle"), 1200);
    } catch {
      setCopyState("error");
    }
  }

  async function runDeployment() {
    setDeploymentStatus("Wallet deployment requested.");
  }

  if (!isStarted) {
    return (
      <section className="my-8 rounded-2xl border border-white/10 bg-base-900/70 p-6 shadow-2xl shadow-brand-500/10">
        <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-[0.2em] text-brand-300">
          <span className="h-2 w-2 rounded-full bg-brand-400" />
          Issuer onboarding
        </div>
        <h2 className="mt-4 text-3xl font-bold text-base-50">Launch a compliant token issuance workflow</h2>
        <p className="mt-3 max-w-2xl text-base text-base-200/75">
          Guide new issuers through token metadata, compliance setup, allowlist seeding, and deployment scripts without losing progress between refreshes.
        </p>
        <button
          type="button"
          onClick={() => setIsStarted(true)}
          className="mt-6 inline-flex items-center rounded-xl bg-brand-500 px-5 py-3 text-sm font-semibold text-base-950 transition hover:bg-brand-400"
        >
          Start onboarding
        </button>
      </section>
    );
  }

  return (
    <section className="my-8 rounded-2xl border border-white/10 bg-base-900/70 p-6 shadow-2xl shadow-brand-500/10">
      <div className="flex flex-col gap-3 border-b border-white/10 pb-5 md:flex-row md:items-center md:justify-between">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.2em] text-brand-300">Issuer workflow</p>
          <h2 className="mt-2 text-2xl font-bold text-base-50">{currentStep.title}</h2>
        </div>
        <div className="flex items-center gap-2">
          {steps.map((step, index) => {
            const active = index === stepIndex;
            const complete = index < stepIndex;
            return (
              <div key={step.id} className="flex items-center gap-2">
                <div
                  className={`flex h-8 w-8 items-center justify-center rounded-full border text-xs font-semibold ${
                    active
                      ? "border-brand-400 bg-brand-500 text-base-950"
                      : complete
                        ? "border-emerald-400 bg-emerald-500/20 text-emerald-300"
                        : "border-white/10 bg-base-850 text-base-300"
                  }`}
                >
                  {index + 1}
                </div>
                {index < steps.length - 1 && <div className="h-px w-6 bg-white/10" />}
              </div>
            );
          })}
        </div>
      </div>

      <p className="mt-4 text-sm text-base-200/70">{currentStep.description}</p>

      {stepIndex === 0 && (
        <div className="mt-6 grid gap-4 md:grid-cols-2">
          <label htmlFor="assetName" className="block text-sm text-base-200">
            <span className="mb-2 block font-medium">Asset name</span>
            <input
              id="assetName"
              {...form.register("assetName")}
              aria-invalid={Boolean(form.formState.errors.assetName)}
              className="w-full rounded-xl border border-white/10 bg-base-950 px-3 py-2.5 text-base-100 outline-none transition focus:border-brand-400"
              placeholder="Bluebird Capital"
            />
            {form.formState.errors.assetName && (
              <span className="mt-1 block text-xs text-red-300">{form.formState.errors.assetName.message}</span>
            )}
          </label>

          <label htmlFor="ticker" className="block text-sm text-base-200">
            <span className="mb-2 block font-medium">Ticker symbol</span>
            <input
              id="ticker"
              {...form.register("ticker")}
              aria-invalid={Boolean(form.formState.errors.ticker)}
              className="w-full rounded-xl border border-white/10 bg-base-950 px-3 py-2.5 text-base-100 outline-none transition focus:border-brand-400"
              placeholder="BRC"
            />
            {form.formState.errors.ticker && (
              <span className="mt-1 block text-xs text-red-300">{form.formState.errors.ticker.message}</span>
            )}
          </label>

          <label htmlFor="issuerPublicKey" className="block text-sm text-base-200 md:col-span-2">
            <span className="mb-2 block font-medium">Issuer public key</span>
            <input
              id="issuerPublicKey"
              {...form.register("issuerPublicKey")}
              aria-invalid={Boolean(form.formState.errors.issuerPublicKey)}
              className="w-full rounded-xl border border-white/10 bg-base-950 px-3 py-2.5 font-mono text-sm text-base-100 outline-none transition focus:border-brand-400"
              placeholder="G..."
            />
            {form.formState.errors.issuerPublicKey && (
              <span className="mt-1 block text-xs text-red-300">{form.formState.errors.issuerPublicKey.message}</span>
            )}
          </label>

          <label htmlFor="totalSupply" className="block text-sm text-base-200 md:col-span-2">
            <span className="mb-2 block font-medium">Total supply</span>
            <input
              id="totalSupply"
              {...form.register("totalSupply")}
              aria-invalid={Boolean(form.formState.errors.totalSupply)}
              type="number"
              min="1"
              className="w-full rounded-xl border border-white/10 bg-base-950 px-3 py-2.5 text-base-100 outline-none transition focus:border-brand-400"
              placeholder="1000000"
            />
            {form.formState.errors.totalSupply && (
              <span className="mt-1 block text-xs text-red-300">{form.formState.errors.totalSupply.message}</span>
            )}
          </label>
        </div>
      )}

      {stepIndex === 1 && (
        <div className="mt-6 grid gap-4 md:grid-cols-2">
          <label htmlFor="complianceOfficerKey" className="block text-sm text-base-200 md:col-span-2">
            <span className="mb-2 block font-medium">Compliance officer key</span>
            <input
              id="complianceOfficerKey"
              {...form.register("complianceOfficerKey")}
              aria-invalid={Boolean(form.formState.errors.complianceOfficerKey)}
              className="w-full rounded-xl border border-white/10 bg-base-950 px-3 py-2.5 font-mono text-sm text-base-100 outline-none transition focus:border-brand-400"
              placeholder="G..."
            />
            {form.formState.errors.complianceOfficerKey && (
              <span className="mt-1 block text-xs text-red-300">{form.formState.errors.complianceOfficerKey.message}</span>
            )}
          </label>

          <label htmlFor="jurisdiction" className="block text-sm text-base-200">
            <span className="mb-2 block font-medium">Jurisdiction</span>
            <input
              id="jurisdiction"
              {...form.register("jurisdiction")}
              aria-invalid={Boolean(form.formState.errors.jurisdiction)}
              className="w-full rounded-xl border border-white/10 bg-base-950 px-3 py-2.5 text-base-100 outline-none transition focus:border-brand-400"
              placeholder="US"
            />
            {form.formState.errors.jurisdiction && (
              <span className="mt-1 block text-xs text-red-300">{form.formState.errors.jurisdiction.message}</span>
            )}
          </label>

          <label htmlFor="allowlistSeed" className="block text-sm text-base-200">
            <span className="mb-2 block font-medium">Allowlist seed</span>
            <input
              id="allowlistSeed"
              {...form.register("allowlistSeed")}
              aria-invalid={Boolean(form.formState.errors.allowlistSeed)}
              className="w-full rounded-xl border border-white/10 bg-base-950 px-3 py-2.5 font-mono text-sm text-base-100 outline-none transition focus:border-brand-400"
              placeholder="G..."
            />
            {form.formState.errors.allowlistSeed && (
              <span className="mt-1 block text-xs text-red-300">{form.formState.errors.allowlistSeed.message}</span>
            )}
          </label>
        </div>
      )}

      {stepIndex === 2 && (
        <div className="mt-6 space-y-5">
          <div className="rounded-xl border border-emerald-500/20 bg-emerald-500/5 p-4">
            <h3 className="text-lg font-semibold text-emerald-300">Review summary</h3>
            <dl className="mt-3 grid gap-3 text-sm text-base-200 md:grid-cols-2">
              <div>
                <dt className="text-base-300">Asset</dt>
                <dd className="mt-1 font-medium text-base-50">{values.assetName || "Not set"}</dd>
              </div>
              <div>
                <dt className="text-base-300">Ticker</dt>
                <dd className="mt-1 font-medium text-base-50">{values.ticker || "Not set"}</dd>
              </div>
              <div>
                <dt className="text-base-300">Issuer</dt>
                <dd className="mt-1 font-mono text-xs text-base-50">{values.issuerPublicKey || "Not set"}</dd>
              </div>
              <div>
                <dt className="text-base-300">Jurisdiction</dt>
                <dd className="mt-1 font-medium text-base-50">{values.jurisdiction || "Not set"}</dd>
              </div>
            </dl>
          </div>

          <div className="rounded-xl border border-white/10 bg-base-950 p-4">
            <div className="flex items-center justify-between gap-3">
              <h3 className="text-lg font-semibold text-base-50">Deployment script</h3>
              <button
                type="button"
                onClick={handleCopy}
                className="rounded-lg border border-white/10 bg-base-900 px-3 py-1.5 text-xs font-medium text-base-100 transition hover:border-brand-400"
              >
                {copyState === "copied" ? "Copied" : "Copy script"}
              </button>
            </div>
            <pre className="mt-3 overflow-x-auto rounded-lg border border-white/10 bg-base-950 p-3 text-xs leading-6 text-brand-100">
              <code>{deploymentScript}</code>
            </pre>
          </div>

          <div className="flex flex-wrap items-center gap-3">
            <button
              type="button"
              onClick={runDeployment}
              className="rounded-xl bg-brand-500 px-5 py-3 text-sm font-semibold text-base-950 transition hover:bg-brand-400"
            >
              Trigger wallet deployment
            </button>
            {deploymentStatus && <span className="text-sm text-emerald-300">{deploymentStatus}</span>}
          </div>
        </div>
      )}

      <div className="mt-8 flex items-center justify-between gap-3 border-t border-white/10 pt-5">
        <button
          type="button"
          onClick={() => setIsStarted(false)}
          className="rounded-lg border border-white/10 bg-base-900 px-4 py-2 text-sm text-base-200 transition hover:border-brand-400"
        >
          Exit
        </button>

        <div className="flex items-center gap-3">
          {stepIndex > 0 && (
            <button
              type="button"
              onClick={previousStep}
              className="rounded-lg border border-white/10 bg-base-900 px-4 py-2 text-sm text-base-200 transition hover:border-brand-400"
            >
              Back
            </button>
          )}

          {stepIndex < steps.length - 1 && (
            <button
              type="button"
              onClick={nextStep}
              className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-base-950 transition hover:bg-brand-400"
            >
              Next
            </button>
          )}
        </div>
      </div>
    </section>
  );
}
