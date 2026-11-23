"use client";

/**
 * Interactive SEP-24 deposit / KYC form.
 *
 * Flow (mirrors the API): submit KYC -> confirm the external transfer -> read
 * the transaction back to obtain the signed XDR. The XDR is only displayed:
 * this demo NEVER signs or submits a Stellar transaction on the user's behalf.
 */

import { useEffect, useState, type FormEvent } from "react";
import { z } from "zod";
import { sep24, Sep24ApiError, type Sep24Transaction } from "@/lib/sep24";

const KYC_FIELDS = [
  { name: "first_name", label: "First name", autoComplete: "given-name" },
  { name: "last_name", label: "Last name", autoComplete: "family-name" },
  { name: "dob", label: "Date of birth", type: "date", autoComplete: "bday" },
  { name: "country", label: "Country (2-letter code)", autoComplete: "country-name" },
  { name: "id_type", label: "ID type" },
  { name: "id_number", label: "ID number" },
  { name: "tax_id", label: "Tax ID" },
] as const;

type Field = (typeof KYC_FIELDS)[number]["name"];
type Values = Record<Field, string>;

const EMPTY_VALUES: Values = {
  first_name: "",
  last_name: "",
  dob: "",
  country: "",
  id_type: "",
  id_number: "",
  tax_id: "",
};

const ID_TYPES = [
  { value: "passport", label: "Passport" },
  { value: "national_id", label: "National ID" },
  { value: "driver_license", label: "Driver's licence" },
  { value: "residence_permit", label: "Residence permit" },
] as const;

const kycSchema = z.object({
  first_name: z.string().trim().min(1, "First name is required"),
  last_name: z.string().trim().min(1, "Last name is required"),
  dob: z.string().regex(/^\d{4}-\d{2}-\d{2}$/, "Use YYYY-MM-DD"),
  country: z.string().trim().regex(/^[A-Za-z]{2}$/, "Use a two-letter country code"),
  id_type: z.string().trim().min(1, "Select an ID type"),
  id_number: z.string().trim().min(1, "ID number is required"),
  tax_id: z.string().trim().min(1, "Tax ID is required"),
});

export interface DepositFormProps {
  transactionId: string;
  token: string;
  lang: string;
}

const INPUT_CLASS =
  "mt-1 w-full rounded-lg border border-white/15 bg-base-900 px-3 py-2 text-sm text-base-100 placeholder:text-base-400 focus:border-brand-400 focus:outline-none focus:ring-2 focus:ring-brand-400/40 aria-[invalid=true]:border-red-500";

function StatusChip({ status }: { status: string }) {
  const tone =
    status === "completed"
      ? "bg-emerald-500/15 text-emerald-300 ring-emerald-500/30"
      : status === "error"
        ? "bg-red-500/15 text-red-300 ring-red-500/30"
        : "bg-amber-500/15 text-amber-300 ring-amber-500/30";
  return (
    <span
      role="status"
      aria-live="polite"
      className={`inline-flex items-center rounded-full px-3 py-1 text-xs font-semibold ring-1 ${tone}`}
    >
      {status}
    </span>
  );
}

export function DepositForm({ transactionId, token, lang }: DepositFormProps) {
  const [values, setValues] = useState<Values>(EMPTY_VALUES);
  const [errors, setErrors] = useState<Partial<Record<Field, string>>>({});
  const [transaction, setTransaction] = useState<Sep24Transaction | null>(null);
  const [phase, setPhase] = useState<"editing" | "submitting" | "submitted" | "error">(
    "editing",
  );
  const [errorMessage, setErrorMessage] = useState<string | null>(null);

  // Poll the authoritative transaction status every 5s so external state
  // changes made by the compliance service are reflected without a reload.
  useEffect(() => {
    if (!transactionId || !token) return;
    let active = true;
    const poll = async () => {
      try {
        const next = await sep24.getTransaction(transactionId, token);
        if (active) setTransaction(next);
      } catch {
        // Polling is best-effort; the form's own errors are shown separately.
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), 5000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [transactionId, token]);

  if (!transactionId || !token) {
    return (
      <div className="rounded-xl border border-amber-500/30 bg-amber-500/10 p-6 text-sm text-amber-200">
        This interactive deposit link is missing its <code>transaction_id</code> or{" "}
        <code>token</code>. Return to the anchor&apos;s deposit page to start a new session.
      </div>
    );
  }

  const setValue = (field: Field, value: string) => {
    setValues((current) => ({ ...current, [field]: value }));
    setErrors((current) => ({ ...current, [field]: undefined }));
  };

  const onSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const parsed = kycSchema.safeParse(values);
    if (!parsed.success) {
      const fieldErrors: Partial<Record<Field, string>> = {};
      for (const issue of parsed.error.issues) {
        const field = issue.path[0] as Field | undefined;
        if (field && !fieldErrors[field]) fieldErrors[field] = issue.message;
      }
      setErrors(fieldErrors);
      return;
    }

    setPhase("submitting");
    setErrorMessage(null);
    try {
      await sep24.submitKyc(transactionId, parsed.data);
      await sep24.confirmExternal(transactionId);
      const next = await sep24.getTransaction(transactionId, token);
      setTransaction(next);
      setPhase("submitted");
    } catch (cause) {
      setErrorMessage(
        cause instanceof Sep24ApiError || cause instanceof Error
          ? cause.message
          : "Could not complete the deposit request.",
      );
      setPhase("error");
    }
  };

  return (
    <section className="mx-auto max-w-2xl space-y-6" lang={lang}>
      <header className="space-y-2">
        <h1 className="text-2xl font-bold text-base-50">Complete your deposit</h1>
        <p className="text-sm text-base-300">
          Provide the identity information required by the anchor. Your details are used only
          to satisfy KYC / AML obligations and to register you on the compliance allowlist.
        </p>
      </header>

      <div className="flex items-center gap-3 rounded-lg border border-white/10 bg-base-900/60 p-3">
        <span className="text-xs uppercase tracking-wide text-base-400">Transaction</span>
        <code className="truncate text-sm text-brand-200">{transactionId}</code>
        {transaction ? <StatusChip status={transaction.status} /> : null}
      </div>

      {phase !== "submitted" ? (
        <form onSubmit={onSubmit} noValidate className="space-y-5">
          {KYC_FIELDS.map((field) => {
            const error = errors[field.name];
            const describedBy = error ? `${field.name}-error` : undefined;
            return (
              <div key={field.name}>
                <label htmlFor={field.name} className="block text-sm font-medium text-base-200">
                  {field.label} <span aria-hidden="true">*</span>
                </label>
                {field.name === "id_type" ? (
                  <select
                    id={field.name}
                    name={field.name}
                    value={values[field.name]}
                    onChange={(event) => setValue(field.name, event.target.value)}
                    aria-invalid={Boolean(error)}
                    aria-describedby={describedBy}
                    className={INPUT_CLASS}
                  >
                    <option value="">Select an ID type…</option>
                    {ID_TYPES.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    id={field.name}
                    name={field.name}
                    type={"type" in field ? field.type : "text"}
                    autoComplete={"autoComplete" in field ? field.autoComplete : undefined}
                    value={values[field.name]}
                    onChange={(event) => setValue(field.name, event.target.value)}
                    aria-invalid={Boolean(error)}
                    aria-describedby={describedBy}
                    className={INPUT_CLASS}
                  />
                )}
                {error ? (
                  <p id={describedBy} className="mt-1 text-sm text-red-400" role="alert">
                    {error}
                  </p>
                ) : null}
              </div>
            );
          })}

          {errorMessage ? (
            <p className="rounded-lg border border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300" role="alert">
              {errorMessage}
            </p>
          ) : null}

          <button
            type="submit"
            disabled={phase === "submitting"}
            className="rounded-lg bg-brand-500 px-5 py-2.5 text-sm font-semibold text-base-950 transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:opacity-60"
          >
            {phase === "submitting" ? "Submitting…" : "Submit KYC & continue"}
          </button>
        </form>
      ) : (
        <div className="space-y-4">
          <div className="rounded-lg border border-emerald-500/30 bg-emerald-500/10 p-4 text-sm text-emerald-200">
            Your KYC has been recorded and the external transfer has been confirmed. The anchor
            is preparing your signed Stellar transaction.
          </div>

          <div>
            <h2 className="mb-2 text-lg font-semibold text-base-100">Submit to Stellar</h2>
            {transaction?.signed_transaction ? (
              <>
                <p className="mb-2 text-sm text-base-300">
                  Copy the base64 XDR envelope below, sign it with your Stellar wallet (or{" "}
                  <code>stellar transaction sign</code>), then submit it to the network yourself.
                  This demo never submits on your behalf.
                </p>
                <pre
                  aria-label="Signed Stellar transaction XDR"
                  className="max-h-48 overflow-auto rounded-lg border border-white/10 bg-base-950 p-3 text-xs break-all text-brand-200"
                >
                  {transaction.signed_transaction}
                </pre>
              </>
            ) : (
              <p className="text-sm text-base-300">
                The signing service has not returned an XDR envelope yet. Keep this page open —
                status refreshes every five seconds. In a production deployment a real signing
                service must be configured (<code>SEP24_SIGNING_SERVICE_URL</code>).
              </p>
            )}
            <button
              type="button"
              disabled
              title="Automatic submission is intentionally disabled in this demo"
              className="mt-3 cursor-not-allowed rounded-lg border border-white/10 px-4 py-2 text-sm text-base-400 opacity-60"
            >
              Submit to Stellar (disabled in demo)
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
