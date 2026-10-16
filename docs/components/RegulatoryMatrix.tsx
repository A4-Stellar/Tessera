"use client";

import { useMemo, useState } from "react";

export type RegulatoryFramework = "reg-d" | "reg-s" | "reg-a-plus" | "mica";

export interface RegulatoryRule {
  id: RegulatoryFramework;
  name: string;
  jurisdiction: string;
  investorCap: string;
  accreditedStatus: string;
  lockupPeriod: string;
  lockupDays: number;
  accreditedOnly: boolean;
  notes: string;
}

export const REGULATORY_RULES: RegulatoryRule[] = [
  {
    id: "reg-d",
    name: "SEC Regulation D",
    jurisdiction: "United States",
    investorCap: "Unlimited accredited investors; up to 35 non-accredited (Rule 506(b))",
    accreditedStatus: "Accredited investors required for Rule 506(c); 506(b) permits 35 sophisticated non-accredited investors",
    lockupPeriod: "12 months (typical restricted-security resale period)",
    lockupDays: 365,
    accreditedOnly: true,
    notes: "Rule 506(c) permits general solicitation when every purchaser is verified as accredited.",
  },
  {
    id: "reg-s",
    name: "SEC Regulation S",
    jurisdiction: "Non-U.S. offshore offering",
    investorCap: "No fixed investor-count cap",
    accreditedStatus: "No U.S. accredited-investor requirement; offers must be made offshore to non-U.S. persons",
    lockupPeriod: "40 days (Category 2 equity/debt distribution compliance period; instrument-dependent)",
    lockupDays: 40,
    accreditedOnly: false,
    notes: "Reg S is not available for directed selling efforts into the United States.",
  },
  {
    id: "reg-a-plus",
    name: "SEC Regulation A+",
    jurisdiction: "United States",
    investorCap: "Tier 1: $20M; Tier 2: $75M in a rolling 12-month period",
    accreditedStatus: "Non-accredited investors permitted, subject to Tier 2 investment limits",
    lockupPeriod: "No statutory lockup; resale and transfer restrictions may apply",
    lockupDays: 0,
    accreditedOnly: false,
    notes: "Tier 2 generally limits non-accredited investors to 10% of annual income or net worth.",
  },
  {
    id: "mica",
    name: "EU MiCA",
    jurisdiction: "European Union",
    investorCap: "No general investor-count cap; exemptions can impose €5M/€100k thresholds",
    accreditedStatus: "Retail investors permitted; qualified-investor exemptions and consumer disclosures may apply",
    lockupPeriod: "No general statutory lockup; token-specific terms and market-abuse rules apply",
    lockupDays: 0,
    accreditedOnly: false,
    notes: "The applicable MiCA title depends on whether the token is an ART, EMT, or other crypto-asset.",
  },
];

const DEFAULT_SELECTED: RegulatoryFramework[] = ["reg-d", "reg-s", "reg-a-plus", "mica"];

function ruleArguments(rules: RegulatoryRule[]) {
  return {
    regulatory_frameworks: rules.map((rule) => rule.id),
    investor_caps: rules.map((rule) => ({
      framework: rule.id,
      description: rule.investorCap,
    })),
    accredited_only: rules.reduce<Record<string, boolean>>(
      (result, rule) => ({ ...result, [rule.id]: rule.accreditedOnly }),
      {},
    ),
    lockup_days: rules.reduce<Record<string, number>>(
      (result, rule) => ({ ...result, [rule.id]: rule.lockupDays }),
      {},
    ),
  };
}

/** An accessible, printable comparison of common token offering frameworks. */
export default function RegulatoryMatrix() {
  const [selected, setSelected] = useState<RegulatoryFramework[]>(DEFAULT_SELECTED);
  const [copied, setCopied] = useState(false);
  const selectedRules = useMemo(
    () => REGULATORY_RULES.filter((rule) => selected.includes(rule.id)),
    [selected],
  );
  const args = useMemo(() => ruleArguments(selectedRules), [selectedRules]);
  const argsJson = useMemo(() => JSON.stringify(args, null, 2), [args]);

  function toggleRule(id: RegulatoryFramework) {
    setSelected((current) =>
      current.includes(id) ? current.filter((item) => item !== id) : [...current, id],
    );
    setCopied(false);
  }

  async function copyArguments() {
    try {
      await navigator.clipboard.writeText(argsJson);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  function exportPdf() {
    if (typeof window !== "undefined") window.print();
  }

  return (
    <section
      aria-labelledby="regulatory-matrix-title"
      className="my-8 rounded-2xl border border-white/10 bg-base-900/80 p-5 shadow-xl sm:p-6"
    >
      <div className="mb-6 flex flex-wrap items-start justify-between gap-4 border-b border-white/10 pb-5">
        <div>
          <p className="text-xs font-semibold uppercase tracking-wider text-brand-300">
            Offering framework selector
          </p>
          <h2 id="regulatory-matrix-title" className="mt-2 text-2xl font-bold text-base-100">
            Regulatory comparison matrix
          </h2>
          <p className="mt-1 max-w-3xl text-sm text-base-300">
            Select the frameworks that apply to your offering, then copy the resulting
            Soroban initialization arguments or print a compliance summary to PDF.
          </p>
        </div>
        <button
          type="button"
          onClick={exportPdf}
          className="rounded-lg border border-brand-400/40 px-3 py-2 text-sm font-semibold text-brand-300 hover:bg-brand-500/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        >
          Export PDF summary
        </button>
      </div>

      <fieldset>
        <legend className="mb-3 text-sm font-semibold text-base-100">Rules to compare</legend>
        <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
          {REGULATORY_RULES.map((rule) => (
            <label
              key={rule.id}
              className="flex cursor-pointer items-start gap-2 rounded-lg border border-white/10 p-3 text-sm text-base-100 hover:border-brand-400/50"
            >
              <input
                type="checkbox"
                checked={selected.includes(rule.id)}
                onChange={() => toggleRule(rule.id)}
                className="mt-0.5 accent-brand-400"
              />
              <span>{rule.name}</span>
            </label>
          ))}
        </div>
      </fieldset>

      <div className="mt-6 overflow-x-auto rounded-xl border border-white/10">
        <table className="min-w-[760px] w-full text-left text-sm">
          <caption className="sr-only">Selected regulatory framework comparison</caption>
          <thead className="bg-white/5 text-xs uppercase tracking-wide text-base-300">
            <tr>
              <th scope="col" className="w-44 px-4 py-3">Requirement</th>
              {selectedRules.map((rule) => (
                <th scope="col" key={rule.id} className="px-4 py-3">{rule.name}</th>
              ))}
            </tr>
          </thead>
          <tbody className="divide-y divide-white/10 text-base-200">
            {[
              ["Investor cap", (rule: RegulatoryRule) => rule.investorCap],
              ["Accredited status", (rule: RegulatoryRule) => rule.accreditedStatus],
              ["Lockup period", (rule: RegulatoryRule) => rule.lockupPeriod],
            ].map(([label, value]) => (
              <tr key={label as string}>
                <th scope="row" className="px-4 py-3 align-top font-semibold text-base-100">{label as string}</th>
                {selectedRules.map((rule) => (
                  <td key={rule.id} className="px-4 py-3 align-top">{(value as (rule: RegulatoryRule) => string)(rule)}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
        {selectedRules.length === 0 && (
          <p className="p-4 text-sm text-base-300">Select at least one framework to populate the matrix.</p>
        )}
      </div>

      <div className="mt-6 grid gap-6 lg:grid-cols-2">
        <div>
          <h3 className="text-lg font-semibold text-base-100">Soroban initialization arguments</h3>
          <p className="mt-1 text-sm text-base-300">
            Pass this JSON to your compliance contract&apos;s <code>initialize</code> call.
          </p>
          <div className="relative mt-3">
            <pre className="max-h-72 overflow-auto rounded-xl bg-[#0a0c11] p-4 text-xs leading-relaxed text-base-100">
              <code>{argsJson}</code>
            </pre>
            <button
              type="button"
              onClick={copyArguments}
              aria-label="Copy Soroban initialization arguments"
              className="absolute right-2 top-2 rounded-md border border-white/20 bg-white/10 px-2 py-1 text-xs text-base-100 hover:bg-white/20 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
            >
              <span aria-live="polite">{copied ? "Copied" : "Copy arguments"}</span>
            </button>
          </div>
        </div>
        <aside aria-label="Compliance summary" className="rounded-xl border border-white/10 bg-white/[.03] p-4">
          <h3 className="text-lg font-semibold text-base-100">Compliance summary</h3>
          <ul className="mt-3 space-y-3 text-sm text-base-200">
            {selectedRules.map((rule) => (
              <li key={rule.id}>
                <strong className="text-base-100">{rule.name}:</strong> {rule.notes}
              </li>
            ))}
            {selectedRules.length === 0 && <li>Select a rule to see its compliance notes.</li>}
          </ul>
          <p className="mt-4 text-xs text-base-300">
            This comparison is informational, not legal advice. Confirm the offering facts and
            current rules with qualified counsel before deployment.
          </p>
        </aside>
      </div>
    </section>
  );
}

export { ruleArguments };
