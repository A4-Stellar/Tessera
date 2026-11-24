"use client";

import { useEffect, useMemo, useState } from "react";
import { z } from "zod";

const STORAGE_KEY = "tessera-issuer-wizard";

type AssetType = "real_estate" | "invoice" | "commodity";
type Step = 0 | 1 | 2 | 3 | 4;
const LAST_STEP: Step = 4;

/** Issue #478: largest board the compliance contract accepts. */
const MAX_BOARD_SIGNERS = 20;
type DeployStatus = "idle" | "deploying" | "success";

type WizardForm = {
  projectName: string;
  assetName: string;
  assetType: AssetType;
  issuerAddress: string;
  totalSupply: string;
  decimals: string;
  jurisdiction: string;
  allowlist: string;
  /** Issue #478: board approval for large transfers. */
  multisigEnabled: boolean;
  multisigThreshold: string;
  multisigRequired: string;
  boardSigners: string;
  complianceNote: string;
};

const defaultState: WizardForm = {
  projectName: "",
  assetName: "",
  assetType: "real_estate",
  issuerAddress: "",
  totalSupply: "1000000",
  decimals: "7",
  jurisdiction: "US",
  allowlist: "",
  multisigEnabled: false,
  multisigThreshold: "100000",
  multisigRequired: "3",
  boardSigners: "",
  complianceNote: "Initial issue approved for issuer and primary admin.",
};

const stellarPublicKeySchema = z
  .string()
  .trim()
  .min(1, "Issuer address is required")
  .regex(/^G[A-Z2-7]{55}$/, "Issuer address must be a valid Stellar public key");

const allowlistSchema = z
  .string()
  .trim()
  .refine((value) => {
    const addresses = value
      .split(/\n|,/)
      .map((item) => item.trim())
      .filter(Boolean);

    if (addresses.length === 0) return false;
    return addresses.every((address) => /^G[A-Z2-7]{55}$/.test(address));
  }, "Add at least one valid Stellar public key for the allowlist.");

const PUBLIC_KEY = /^G[A-Z2-7]{55}$/;

/** Issue #478: m-of-n board policy, validated like `set_multisig_policy`. */
const multisigSchema = z
  .object({
    multisigEnabled: z.boolean(),
    multisigThreshold: z.string(),
    multisigRequired: z.string(),
    boardSigners: z.string(),
  })
  .superRefine((value, ctx) => {
    if (!value.multisigEnabled) return;
    const threshold = Number(value.multisigThreshold);
    if (!Number.isFinite(threshold) || threshold <= 0) {
      ctx.addIssue({ code: "custom", path: ["multisigThreshold"], message: "The approval threshold must be greater than zero." });
    }
    const signers = parseAllowlist(value.boardSigners);
    if (signers.length === 0 || !signers.every((s) => PUBLIC_KEY.test(s))) {
      ctx.addIssue({ code: "custom", path: ["boardSigners"], message: "Add at least one valid Stellar public key per board signer." });
    } else if (new Set(signers).size !== signers.length) {
      ctx.addIssue({ code: "custom", path: ["boardSigners"], message: "Each board signer can only be listed once." });
    } else if (signers.length > MAX_BOARD_SIGNERS) {
      ctx.addIssue({ code: "custom", path: ["boardSigners"], message: `A board can have at most ${MAX_BOARD_SIGNERS} signers.` });
    }
    const required = Number(value.multisigRequired);
    if (!Number.isInteger(required) || required < 1 || (signers.length > 0 && required > signers.length)) {
      ctx.addIssue({
        code: "custom",
        path: ["multisigRequired"],
        message: `Required signatures must be a whole number between 1 and ${Math.max(signers.length, 1)}.`,
      });
    }
  });

const stepSchemas: Record<Step, z.ZodTypeAny> = {
  0: z.object({
    projectName: z.string().trim().min(2, "Project name must be at least 2 characters."),
    assetName: z.string().trim().min(3, "Asset name must be at least 3 characters."),
    assetType: z.enum(["real_estate", "invoice", "commodity"]),
    issuerAddress: stellarPublicKeySchema,
  }),
  1: z.object({
    totalSupply: z.coerce.number().positive("Total supply must be greater than zero."),
    decimals: z.coerce.number().int("Decimals must be a whole number.").min(0, "Decimals cannot be negative.").max(18, "Decimals cannot exceed 18."),
    jurisdiction: z.string().trim().min(2, "Jurisdiction is required."),
    allowlist: allowlistSchema,
  }),
  2: multisigSchema,
  3: z.object({
    complianceNote: z.string().trim().min(10, "Add a brief compliance note."),
  }),
  4: z.object({}),
};

const formSummary = [
  "Project details",
  "Token configuration",
  "Board approvals",
  "Compliance review",
  "Deployment",
] as const;

function parseAllowlist(raw: string) {
  return raw
    .split(/\n|,/)
    .map((item) => item.trim())
    .filter(Boolean);
}

function extractError(issues: z.ZodIssue[]) {
  return issues[0]?.message ?? "Please review the form.";
}

export function IssuerWizard() {
  const [form, setForm] = useState<WizardForm>(defaultState);
  const [step, setStep] = useState<Step>(0);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [hydrated, setHydrated] = useState(false);
  const [deployStatus, setDeployStatus] = useState<DeployStatus>("idle");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    try {
      const raw = window.localStorage.getItem(STORAGE_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as Partial<WizardForm>;
        setForm({ ...defaultState, ...parsed });
      }
    } catch {
      // Ignore malformed localStorage payloads and keep defaults.
    } finally {
      setHydrated(true);
    }
  }, []);

  useEffect(() => {
    if (!hydrated) return;
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(form));
  }, [form, hydrated]);

  const updateField = <K extends keyof WizardForm>(field: K, value: WizardForm[K]) => {
    setForm((current) => ({ ...current, [field]: value }));
    setErrors((current) => ({ ...current, [field]: "" }));
  };

  const currentStepIndex = step;

  const validateStep = (targetStep: Step) => {
    const parsed = stepSchemas[targetStep].safeParse(
      targetStep === 0
        ? {
            projectName: form.projectName,
            assetName: form.assetName,
            assetType: form.assetType,
            issuerAddress: form.issuerAddress,
          }
        : targetStep === 1
          ? {
              totalSupply: form.totalSupply,
              decimals: form.decimals,
              jurisdiction: form.jurisdiction,
              allowlist: form.allowlist,
            }
          : targetStep === 2
            ? {
                multisigEnabled: form.multisigEnabled,
                multisigThreshold: form.multisigThreshold,
                multisigRequired: form.multisigRequired,
                boardSigners: form.boardSigners,
              }
            : {
                complianceNote: form.complianceNote,
              }
    );

    if (!parsed.success) {
      const nextErrors = parsed.error.issues.reduce<Record<string, string>>((acc: Record<string, string>, issue: z.ZodIssue) => {
        const name = issue.path[0]?.toString() ?? "form";
        acc[name] = extractError([issue]);
        return acc;
      }, {});
      setErrors(nextErrors);
      return false;
    }

    setErrors({});
    return true;
  };

  const deploymentScript = useMemo(
    () => `# tessera issuer onboarding script
export ASSET_NAME="${form.assetName || "My Asset"}"
export PROJECT_NAME="${form.projectName || "My Project"}"
export ISSUER_ADDRESS="${form.issuerAddress || "G..."}"
export ASSET_TYPE="${form.assetType}"
export TOTAL_SUPPLY="${form.totalSupply || "1000000"}"
export DECIMALS="${form.decimals || "7"}"
export JURISDICTION="${form.jurisdiction || "US"}"
export ALLOWLIST="${parseAllowlist(form.allowlist || form.issuerAddress).join(" ") || "G..."}"

cargo run --bin issuer -- deploy \
  --issuer "$ISSUER_ADDRESS" \
  --asset "$ASSET_NAME" \
  --type "$ASSET_TYPE" \
  --supply "$TOTAL_SUPPLY" \
  --decimals "$DECIMALS" \
  --jurisdiction "$JURISDICTION" \
  --allowlist "$ALLOWLIST"${
    form.multisigEnabled
      ? `

# Board approval (m-of-n) for transfers above the threshold (issue #478).
# COMPLIANCE_CONTRACT and ASSET_TOKEN_CONTRACT are the ids printed by the deploy above.
stellar contract invoke --id "$COMPLIANCE_CONTRACT" --source "$ISSUER_ADDRESS" -- \\
  set_multisig_policy \\
  --admin "$ISSUER_ADDRESS" \\
  --token "$ASSET_TOKEN_CONTRACT" \\
  --threshold_amount ${form.multisigThreshold || "0"} \\
  --required_signatures ${form.multisigRequired || "1"} \\
  --board '${JSON.stringify(parseAllowlist(form.boardSigners))}' \\
  --expiry_ledgers 17280`
      : ""
  }`,
    [form]
  );

  const nextStep = () => {
    if (!validateStep(step as Step)) return;
    setStep((current) => (Math.min(current + 1, LAST_STEP) as Step));
  };

  const previousStep = () => {
    setStep((current) => (Math.max(current - 1, 0) as Step));
  };

  const completeWizard = () => {
    if (!validateStep(2) || !validateStep(3)) return;
    setDeployStatus("deploying");
    setTimeout(() => setDeployStatus("success"), 600);
  };

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(deploymentScript);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  };

  const isLastStep = currentStepIndex === LAST_STEP;

  return (
    <div className="my-12 rounded-2xl border border-white/10 bg-base-900/70 p-6 shadow-2xl shadow-brand-500/5">
      <div className="mb-6 flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
        <div>
          <p className="text-sm font-medium uppercase tracking-[0.2em] text-brand-300">Issuer onboarding</p>
          <h2 className="mt-2 text-2xl font-semibold text-white">Tessera compliance workflow canvas</h2>
        </div>
        <div className="inline-flex items-center rounded-full border border-brand-500/30 bg-brand-500/10 px-3 py-1 text-xs font-semibold text-brand-200">
          {formSummary[currentStepIndex]}
        </div>
      </div>

      <div className="mb-8 grid gap-3 md:grid-cols-5">
        {formSummary.map((title, index) => {
          const isActive = index === currentStepIndex;
          const isComplete = index < currentStepIndex;
          return (
            <div
              key={title}
              className={`rounded-xl border px-3 py-2 text-sm ${
                isActive
                  ? "border-brand-500/40 bg-brand-500/10 text-brand-100"
                  : isComplete
                    ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-200"
                    : "border-white/10 bg-white/5 text-base-200/70"
              }`}
            >
              <div className="flex items-center gap-2">
                <span className="flex h-6 w-6 items-center justify-center rounded-full bg-black/20 text-xs font-semibold">
                  {index + 1}
                </span>
                {title}
              </div>
            </div>
          );
        })}
      </div>

      {currentStepIndex === 0 && (
        <div className="grid gap-6 md:grid-cols-2">
          <label className="flex flex-col gap-2 text-sm text-base-100">
            Project name
            <input
              aria-label="Project name"
              value={form.projectName}
              onChange={(event) => updateField("projectName", event.target.value)}
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
              placeholder="Atlas Capital"
            />
            {errors.projectName && <span className="text-xs text-red-300">{errors.projectName}</span>}
          </label>

          <label className="flex flex-col gap-2 text-sm text-base-100">
            Asset name
            <input
              aria-label="Asset name"
              value={form.assetName}
              onChange={(event) => updateField("assetName", event.target.value)}
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
              placeholder="Atlas Residences"
            />
            {errors.assetName && <span className="text-xs text-red-300">{errors.assetName}</span>}
          </label>

          <label className="flex flex-col gap-2 text-sm text-base-100">
            Asset type
            <select
              value={form.assetType}
              onChange={(event) => updateField("assetType", event.target.value as AssetType)}
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
            >
              <option value="real_estate">Real Estate</option>
              <option value="invoice">Invoice</option>
              <option value="commodity">Commodity</option>
            </select>
          </label>

          <label className="flex flex-col gap-2 text-sm text-base-100">
            Issuer address
            <input
              aria-label="Issuer address"
              value={form.issuerAddress}
              onChange={(event) => updateField("issuerAddress", event.target.value)}
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 font-mono text-white outline-none transition focus:border-brand-500"
              placeholder="G..."
            />
            {errors.issuerAddress && <span className="text-xs text-red-300">{errors.issuerAddress}</span>}
          </label>
        </div>
      )}

      {currentStepIndex === 1 && (
        <div className="grid gap-6 md:grid-cols-2">
          <label className="flex flex-col gap-2 text-sm text-base-100">
            Total supply
            <input
              type="number"
              min="1"
              value={form.totalSupply}
              onChange={(event) => updateField("totalSupply", event.target.value)}
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
            />
            {errors.totalSupply && <span className="text-xs text-red-300">{errors.totalSupply}</span>}
          </label>

          <label className="flex flex-col gap-2 text-sm text-base-100">
            Decimals
            <input
              type="number"
              min="0"
              max="18"
              value={form.decimals}
              onChange={(event) => updateField("decimals", event.target.value)}
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
            />
            {errors.decimals && <span className="text-xs text-red-300">{errors.decimals}</span>}
          </label>

          <label className="flex flex-col gap-2 text-sm text-base-100">
            Jurisdiction
            <input
              value={form.jurisdiction}
              onChange={(event) => updateField("jurisdiction", event.target.value)}
              className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
              placeholder="US"
            />
            {errors.jurisdiction && <span className="text-xs text-red-300">{errors.jurisdiction}</span>}
          </label>

          <div className="md:col-span-2">
            <label className="flex flex-col gap-2 text-sm text-base-100">
              Seed allowlist
              <textarea
                value={form.allowlist}
                onChange={(event) => updateField("allowlist", event.target.value)}
                className="min-h-[140px] rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 font-mono text-white outline-none transition focus:border-brand-500"
                placeholder={"G...\nG...\nG..."}
              />
              {errors.allowlist && <span className="text-xs text-red-300">{errors.allowlist}</span>}
            </label>
          </div>
        </div>
      )}

      {currentStepIndex === 2 && (
        <div className="space-y-6">
          <div className="rounded-xl border border-brand-500/25 bg-brand-500/5 p-4">
            <h3 className="text-lg font-semibold text-white">Board approval for large transfers</h3>
            <p className="mt-2 text-sm text-base-200/80">
              Transfers above the threshold are held as pending requests for 24 hours and settle only after the required
              number of board members co-sign them. Smaller transfers are not affected.
            </p>
          </div>

          <label className="flex items-center gap-3 text-sm text-base-100">
            <input
              type="checkbox"
              checked={form.multisigEnabled}
              onChange={(event) => updateField("multisigEnabled", event.target.checked)}
            />
            Require board approval above a threshold
          </label>

          {form.multisigEnabled && (
            <div className="grid gap-6 md:grid-cols-2">
              <label className="flex flex-col gap-2 text-sm text-base-100">
                Approval threshold (token units)
                <input
                  type="number"
                  min="1"
                  value={form.multisigThreshold}
                  onChange={(event) => updateField("multisigThreshold", event.target.value)}
                  className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
                />
                {errors.multisigThreshold && <span className="text-xs text-red-300">{errors.multisigThreshold}</span>}
              </label>

              <label className="flex flex-col gap-2 text-sm text-base-100">
                Required signatures (m)
                <input
                  type="number"
                  min="1"
                  max={MAX_BOARD_SIGNERS}
                  value={form.multisigRequired}
                  onChange={(event) => updateField("multisigRequired", event.target.value)}
                  className="rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
                />
                {errors.multisigRequired && <span className="text-xs text-red-300">{errors.multisigRequired}</span>}
              </label>

              <div className="md:col-span-2">
                <label className="flex flex-col gap-2 text-sm text-base-100">
                  Board signers (n)
                  <textarea
                    value={form.boardSigners}
                    onChange={(event) => updateField("boardSigners", event.target.value)}
                    className="min-h-[120px] rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 font-mono text-white outline-none transition focus:border-brand-500"
                    placeholder={"G...\nG...\nG..."}
                  />
                  {errors.boardSigners && <span className="text-xs text-red-300">{errors.boardSigners}</span>}
                </label>
                <p className="mt-2 text-xs text-base-200/70" aria-live="polite">
                  {`${form.multisigRequired || "?"} of ${parseAllowlist(form.boardSigners).length} board signatures required`}
                </p>
              </div>
            </div>
          )}
        </div>
      )}

      {currentStepIndex === 3 && (
        <div className="space-y-6">
          <div className="rounded-xl border border-brand-500/25 bg-brand-500/5 p-4">
            <h3 className="text-lg font-semibold text-white">Compliance gate summary</h3>
            <ul className="mt-3 space-y-2 text-sm text-base-200/80">
              <li>• The issuer address must be present in the allowlist before the initial mint.</li>
              <li>• Transfers are blocked when jurisdiction, expiry, or KYC status fails compliance.</li>
              <li>• Transaction approvals should be reviewed before deployment.</li>
            </ul>
          </div>

          <label className="flex flex-col gap-2 text-sm text-base-100">
            Compliance note
            <textarea
              value={form.complianceNote}
              onChange={(event) => updateField("complianceNote", event.target.value)}
              className="min-h-[120px] rounded-xl border border-white/10 bg-base-950/80 px-3 py-2 text-white outline-none transition focus:border-brand-500"
              placeholder="Note any resale limit, jurisdiction block, or investor restrictions."
            />
            {errors.complianceNote && <span className="text-xs text-red-300">{errors.complianceNote}</span>}
          </label>
        </div>
      )}

      {currentStepIndex === LAST_STEP && (
        <div className="space-y-6">
          <div className="grid gap-4 md:grid-cols-2">
            <div className="rounded-xl border border-white/10 bg-base-950/70 p-4">
              <p className="text-xs uppercase tracking-[0.2em] text-brand-300">Project</p>
              <p className="mt-2 text-lg font-semibold text-white">{form.projectName || "Untitled project"}</p>
            </div>
            <div className="rounded-xl border border-white/10 bg-base-950/70 p-4">
              <p className="text-xs uppercase tracking-[0.2em] text-brand-300">Issuer</p>
              <p className="mt-2 break-all font-mono text-sm text-base-100">{form.issuerAddress || "Not set"}</p>
            </div>
          </div>

          <div className="rounded-xl border border-white/10 bg-base-950/70 p-4">
            <div className="mb-3 flex items-center justify-between gap-3">
              <h3 className="text-lg font-semibold text-white">Deployment script</h3>
              <button
                type="button"
                onClick={handleCopy}
                className="rounded-lg border border-brand-500/40 bg-brand-500/10 px-3 py-1.5 text-sm font-medium text-brand-100 hover:bg-brand-500/20"
              >
                {copied ? "Copied" : "Copy script"}
              </button>
            </div>
            <pre className="overflow-x-auto rounded-lg border border-white/10 bg-black/20 p-4 text-xs leading-6 text-base-100">
              {deploymentScript}
            </pre>
          </div>

          <div className="flex flex-wrap items-center gap-3">
            <button
              type="button"
              onClick={completeWizard}
              className="rounded-xl bg-brand-500 px-5 py-2.5 text-sm font-semibold text-base-950 transition hover:bg-brand-400"
            >
              {deployStatus === "deploying" ? "Deploying..." : deployStatus === "success" ? "Deployed" : "Deploy via wallet"}
            </button>
            {deployStatus === "success" && (
              <span className="rounded-full border border-emerald-500/30 bg-emerald-500/10 px-3 py-1 text-xs font-medium text-emerald-200">
                Deployment transaction prepared for Freighter.
              </span>
            )}
          </div>
        </div>
      )}

      <div className="mt-8 flex items-center justify-between gap-3 border-t border-white/10 pt-5">
        <button
          type="button"
          onClick={previousStep}
          disabled={step === 0}
          className="rounded-xl border border-white/10 px-4 py-2 text-sm font-medium text-base-100 disabled:cursor-not-allowed disabled:opacity-50"
        >
          Back
        </button>

        {!isLastStep ? (
          <button
            type="button"
            onClick={nextStep}
            className="rounded-xl bg-brand-500 px-5 py-2.5 text-sm font-semibold text-base-950 transition hover:bg-brand-400"
          >
            Continue
          </button>
        ) : (
          <button
            type="button"
            onClick={previousStep}
            className="rounded-xl border border-white/10 bg-white/5 px-4 py-2 text-sm font-medium text-base-100"
          >
            Review again
          </button>
        )}
      </div>
    </div>
  );
}

export default IssuerWizard;
