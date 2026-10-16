"use client";

import Link from "next/link";
import { useId, useMemo, useState, type ReactNode } from "react";
import {
  EMPTY_ANSWERS,
  evaluateExemptions,
  RULES_AS_OF,
  type ExemptionAnswers,
  type FrameworkResult,
  type FrameworkStatus,
} from "@/lib/exemption-rules";

type ChoiceKey = "issuerLocation" | "instrument" | "stableValue" | "usInvestors" | "euInvestors";
type BooleanKey =
  | "generalSolicitation"
  | "reportingIssuer"
  | "regAExcludedIssuer"
  | "euFewerThan150PerState"
  | "euMinimumTicket100k"
  | "seeksAdmissionToTrading"
  | "artOutstandingUnder5m";
type AmountKey = "usRaiseUsd" | "euRaiseEur";

type Field =
  | { kind: "choice"; key: ChoiceKey; label: string; options: { value: string; label: string }[] }
  | { kind: "boolean"; key: BooleanKey; label: string }
  | { kind: "amount"; key: AmountKey; label: string; currency: "USD" | "EUR" }
  | { kind: "markets"; key: "markets"; label: string };

interface Step {
  id: string;
  title: string;
  fields: (answers: ExemptionAnswers) => Field[];
}

const isUsMarket = (a: ExemptionAnswers) => a.markets.includes("us");
const isSecurity = (a: ExemptionAnswers) => a.instrument !== null && a.instrument !== "crypto-asset";
const isRetailEu = (a: ExemptionAnswers) => a.euInvestors === "retail";

/** The questionnaire as data: each step lists only the questions the earlier answers make relevant. */
const STEPS: Step[] = [
  {
    id: "issuer",
    title: "Issuer",
    fields: () => [
      {
        kind: "choice",
        key: "issuerLocation",
        label: "Where is the issuer organized and principally managed?",
        options: [
          { value: "us", label: "United States" },
          { value: "canada", label: "Canada" },
          { value: "eu", label: "European Union" },
          { value: "other", label: "Elsewhere" },
        ],
      },
      {
        kind: "choice",
        key: "instrument",
        label: "What does the token give holders?",
        options: [
          { value: "equity", label: "Equity-like interest (shares, LLC or fund units, fractional property ownership)" },
          { value: "debt", label: "Debt (notes, bonds, loan participations)" },
          { value: "crypto-asset", label: "Neither — a crypto-asset that is not a security or financial instrument" },
        ],
      },
    ],
  },
  {
    id: "markets",
    title: "Investors",
    fields: () => [{ kind: "markets", key: "markets", label: "Where are the investors you will offer the token to?" }],
  },
  {
    id: "us",
    title: "U.S. offering",
    fields: (a) =>
      a.instrument === "crypto-asset" || (!isUsMarket(a) && a.issuerLocation !== "us")
        ? []
        : [
            ...(isUsMarket(a)
              ? ([
                  {
                    kind: "choice",
                    key: "usInvestors",
                    label: "Which U.S. investors will buy?",
                    options: [
                      { value: "accredited-only", label: "Accredited investors only" },
                      { value: "includes-non-accredited", label: "Includes non-accredited investors" },
                    ],
                  },
                  { kind: "boolean", key: "generalSolicitation", label: "Will you market the offering publicly (advertising, open websites, social media)?" },
                  { kind: "amount", key: "usRaiseUsd", label: "Amount to raise from the offering in the next 12 months", currency: "USD" },
                ] satisfies Field[])
              : []),
            { kind: "boolean", key: "reportingIssuer", label: "Is the issuer an SEC (Exchange Act) reporting company?" },
            ...(isUsMarket(a) && (a.issuerLocation === "us" || a.issuerLocation === "canada")
              ? ([
                  {
                    kind: "boolean",
                    key: "regAExcludedIssuer",
                    label: "Is the issuer an investment company or BDC, a blank-check company or SPAC, or an issuer of fractional oil, gas or mineral rights?",
                  },
                ] satisfies Field[])
              : []),
          ],
  },
  {
    id: "eu",
    title: "EU offering",
    fields: (a) => {
      if (!a.markets.includes("eu")) return [];
      const euInvestors: Field = {
        kind: "choice",
        key: "euInvestors",
        label: "Which EU investors will the offer address?",
        options: [
          { value: "qualified-only", label: "Qualified investors only" },
          { value: "retail", label: "Includes retail investors" },
        ],
      };
      const euRaise: Field = { kind: "amount", key: "euRaiseEur", label: "Total consideration offered in the EU over 12 months", currency: "EUR" };
      const fewerThan150: Field = {
        kind: "boolean",
        key: "euFewerThan150PerState",
        label: "Will the offer reach fewer than 150 non-qualified investors in every Member State?",
      };

      if (isSecurity(a)) {
        return [
          euInvestors,
          euRaise,
          ...(isRetailEu(a)
            ? [fewerThan150, { kind: "boolean", key: "euMinimumTicket100k", label: "Will every investor commit at least €100,000?" } satisfies Field]
            : []),
        ];
      }

      const stableValue: Field = {
        kind: "choice",
        key: "stableValue",
        label: "Does the token aim to keep a stable value by referencing something else?",
        options: [
          { value: "none", label: "No" },
          { value: "single-currency", label: "Yes — one official currency (e.g. EUR)" },
          { value: "other-assets", label: "Yes — other assets, rights or a basket" },
        ],
      };
      if (a.stableValue === "other-assets") {
        return [
          stableValue,
          euInvestors,
          ...(isRetailEu(a)
            ? [
                {
                  kind: "boolean",
                  key: "artOutstandingUnder5m",
                  label: "Will the token's average outstanding value stay at or below €5,000,000 over every 12 months?",
                } satisfies Field,
              ]
            : []),
        ];
      }
      if (a.stableValue !== "none") return [stableValue];
      return [
        stableValue,
        euInvestors,
        euRaise,
        ...(isRetailEu(a) ? [fewerThan150] : []),
        { kind: "boolean", key: "seeksAdmissionToTrading", label: "Do you intend to seek admission to trading on a crypto-asset trading platform?" },
      ];
    },
  },
];

const MARKET_OPTIONS = [
  { value: "us", label: "United States" },
  { value: "eu", label: "European Union" },
  { value: "other", label: "Rest of the world" },
] as const;

function isAnswered(field: Field, answers: ExemptionAnswers): boolean {
  return field.kind === "markets" ? answers.markets.length > 0 : answers[field.key] !== null;
}

/** Only the answers the visible questions asked for, so stale answers never leak into the export. */
function relevantAnswers(steps: { fields: Field[] }[], answers: ExemptionAnswers): ExemptionAnswers {
  const relevant: ExemptionAnswers = { ...EMPTY_ANSWERS };
  for (const { fields } of steps) {
    for (const field of fields) Object.assign(relevant, { [field.key]: answers[field.key] });
  }
  return relevant;
}

const STATUS_STYLES: Record<FrameworkStatus, { label: string; icon: string; className: string }> = {
  eligible: { label: "Eligible", icon: "✓", className: "border-brand-500/40 bg-brand-500/10 text-brand-300" },
  required: { label: "Required", icon: "!", className: "border-gold-500/40 bg-gold-500/10 text-gold-300" },
  ineligible: { label: "Not available", icon: "✕", className: "border-red-500/30 bg-red-500/10 text-red-300" },
};

/** Renders `code` spans in rule text as inline code. */
function withInlineCode(text: string): ReactNode[] {
  return text.split(/(`[^`]+`)/g).map((part, index) =>
    part.startsWith("`") && part.endsWith("`") ? (
      <code key={index} className="rounded bg-white/10 px-1 py-0.5 font-mono text-[0.85em] text-brand-300">
        {part.slice(1, -1)}
      </code>
    ) : (
      part
    )
  );
}

function FieldInput({
  field,
  answers,
  onChange,
}: {
  field: Field;
  answers: ExemptionAnswers;
  onChange: (patch: Partial<ExemptionAnswers>) => void;
}) {
  const id = useId();
  const optionClass = "flex items-start gap-2 rounded-lg border border-white/10 px-3 py-2 text-sm text-base-100 hover:border-brand-500/40";

  if (field.kind === "amount") {
    const value = answers[field.key];
    return (
      <div>
        <label htmlFor={id} className="mb-1 block text-sm font-semibold text-base-50">
          {field.label} ({field.currency})
        </label>
        <input
          id={id}
          type="number"
          inputMode="decimal"
          min={0}
          step="any"
          value={value ?? ""}
          onChange={(event) => {
            const parsed = event.target.value === "" ? null : Number(event.target.value);
            onChange({ [field.key]: parsed !== null && Number.isFinite(parsed) && parsed >= 0 ? parsed : null });
          }}
          className="w-full max-w-xs rounded-md border border-white/10 bg-base-900 px-2.5 py-1.5 text-sm text-base-100 focus:border-brand-500/60 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
        />
      </div>
    );
  }

  const options =
    field.kind === "boolean"
      ? [
          { value: "true", label: "Yes" },
          { value: "false", label: "No" },
        ]
      : field.kind === "markets"
        ? MARKET_OPTIONS
        : field.options;

  return (
    <fieldset>
      <legend className="mb-2 text-sm font-semibold text-base-50">{field.label}</legend>
      <div className="grid gap-2 sm:grid-cols-2">
        {options.map((option) => {
          if (field.kind === "markets") {
            const market = option.value as ExemptionAnswers["markets"][number];
            const checked = answers.markets.includes(market);
            return (
              <label key={option.value} className={optionClass}>
                <input
                  type="checkbox"
                  className="mt-1"
                  checked={checked}
                  onChange={() =>
                    onChange({ markets: checked ? answers.markets.filter((m) => m !== market) : [...answers.markets, market] })
                  }
                />
                {option.label}
              </label>
            );
          }
          const current = answers[field.key];
          const checked = field.kind === "boolean" ? current !== null && String(current) === option.value : current === option.value;
          return (
            <label key={option.value} className={optionClass}>
              <input
                type="radio"
                name={id}
                className="mt-1"
                checked={checked}
                onChange={() => onChange({ [field.key]: field.kind === "boolean" ? option.value === "true" : option.value })}
              />
              {option.label}
            </label>
          );
        })}
      </div>
    </fieldset>
  );
}

function ResultCard({ result }: { result: FrameworkResult }) {
  const status = STATUS_STYLES[result.status];
  return (
    <li className="rounded-lg border border-white/10 bg-base-900/60 p-4" aria-label={result.name}>
      <div className="flex flex-wrap items-center gap-2">
        <h4 className="text-sm font-semibold text-base-50">{result.name}</h4>
        <span className="rounded-full border border-white/10 px-2 py-0.5 text-[0.7rem] text-base-300">{result.jurisdiction}</span>
        <span className={`rounded-full border px-2 py-0.5 text-[0.7rem] font-semibold ${status.className}`}>
          <span aria-hidden="true">{status.icon} </span>
          {status.label}
        </span>
        {result.recommended && result.status !== "ineligible" && (
          <span className="rounded-full border border-sky-500/40 bg-sky-500/10 px-2 py-0.5 text-[0.7rem] font-semibold text-sky-300">
            Recommended
          </span>
        )}
      </div>
      <ul className="mt-2 list-disc space-y-1 pl-5 text-xs text-base-200">
        {result.reasons.map((reason) => (
          <li key={reason}>{withInlineCode(reason)}</li>
        ))}
      </ul>
      {result.status !== "ineligible" && (
        <>
          <h5 className="mt-3 text-xs font-semibold uppercase tracking-wide text-base-300">Requirements</h5>
          <ul className="mt-1 list-disc space-y-1 pl-5 text-xs text-base-200">
            {result.requirements.map((requirement) => (
              <li key={requirement}>{withInlineCode(requirement)}</li>
            ))}
          </ul>
          <h5 className="mt-3 text-xs font-semibold uppercase tracking-wide text-base-300">Tessera contract setup</h5>
          <ul className="mt-1 space-y-1.5 text-xs text-base-200">
            {result.setup.map((step) => (
              <li key={step.text}>
                {withInlineCode(step.text)}{" "}
                <Link href={step.guide.href} className="font-medium text-brand-400 underline underline-offset-2">
                  {step.guide.label}
                </Link>
              </li>
            ))}
          </ul>
        </>
      )}
      <p className="mt-3 text-xs text-base-300">
        Source:{" "}
        <a href={result.source.href} target="_blank" rel="noopener noreferrer" className="text-brand-400 underline underline-offset-2">
          {result.source.label}
        </a>
      </p>
    </li>
  );
}

/**
 * Step-by-step questionnaire that maps an issuer's location, investors, raise
 * size and marketing plans to the applicable U.S. SEC exemptions (Regulation D
 * Rule 506(b)/(c), Regulation S, Regulation A Tier 1/2) and EU regimes
 * (Prospectus Regulation, MiCA), with links to the matching contract setup
 * guides and a JSON export of the assessment.
 */
export function ExemptionChecker() {
  const titleId = useId();
  const [answers, setAnswers] = useState<ExemptionAnswers>(EMPTY_ANSWERS);
  const [stepIndex, setStepIndex] = useState(0);

  const steps = useMemo(
    () => STEPS.map((step) => ({ ...step, fields: step.fields(answers) })).filter((step) => step.fields.length > 0),
    [answers]
  );
  const showResults = stepIndex >= steps.length;
  const step = showResults ? null : steps[stepIndex];
  const stepComplete = step ? step.fields.every((field) => isAnswered(field, answers)) : true;
  const assessment = useMemo(() => (showResults ? evaluateExemptions(answers) : null), [answers, showResults]);

  const update = (patch: Partial<ExemptionAnswers>) => setAnswers((current) => ({ ...current, ...patch }));

  const exportJson = () => {
    if (!assessment) return;
    const report = {
      tool: "Tessera regulatory exemption checker",
      rulesAsOf: RULES_AS_OF,
      generatedAt: new Date().toISOString(),
      disclaimer: "Educational assessment only; not legal advice. Confirm with qualified securities counsel.",
      answers: relevantAnswers(steps, answers),
      ...assessment,
    };
    const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: "application/json" }));
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `exemption-assessment-${report.generatedAt.slice(0, 10)}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  return (
    <section aria-labelledby={titleId} className="my-6 overflow-hidden rounded-xl border border-white/10 bg-white/[0.03]">
      <header className="border-b border-white/10 px-4 py-3">
        <h3 id={titleId} className="text-base font-bold text-base-50">
          Regulatory exemption checker
        </h3>
        <p className="mt-1 text-xs text-base-300">
          Educational guidance on rules in force as of {RULES_AS_OF} — not legal advice. Confirm your plan with securities counsel.
        </p>
      </header>

      <div className="space-y-5 p-4">
        <ol className="flex flex-wrap gap-2 text-xs" aria-label="Progress">
          {[...steps.map((candidate) => candidate.title), "Results"].map((title, index) => (
            <li
              key={title}
              aria-current={index === stepIndex ? "step" : undefined}
              className={`rounded-full border px-2.5 py-1 ${
                index === stepIndex
                  ? "border-brand-500/50 bg-brand-500/10 text-brand-300"
                  : index < stepIndex
                    ? "border-white/10 text-base-100"
                    : "border-white/10 text-base-300"
              }`}
            >
              {index + 1}. {title}
            </li>
          ))}
        </ol>

        {step && (
          <form
            className="space-y-5"
            aria-label={step.title}
            onSubmit={(event) => {
              event.preventDefault();
              if (stepComplete) setStepIndex((index) => index + 1);
            }}
          >
            {step.fields.map((field) => (
              <FieldInput key={field.key} field={field} answers={answers} onChange={update} />
            ))}
            <div className="flex justify-between gap-3 border-t border-white/10 pt-4">
              <button
                type="button"
                onClick={() => setStepIndex((index) => Math.max(index - 1, 0))}
                disabled={stepIndex === 0}
                className="rounded-md border border-white/10 px-4 py-2 text-sm text-base-100 disabled:cursor-not-allowed disabled:opacity-40"
              >
                Back
              </button>
              <button
                type="submit"
                disabled={!stepComplete}
                className="rounded-md bg-brand-500 px-4 py-2 text-sm font-semibold text-base-950 hover:bg-brand-400 disabled:cursor-not-allowed disabled:opacity-40"
              >
                {stepIndex === steps.length - 1 ? "See results" : "Continue"}
              </button>
            </div>
          </form>
        )}

        {assessment && (
          <div className="space-y-4">
            {assessment.results.length > 0 ? (
              <ul className="space-y-3">
                {[...assessment.results]
                  .sort((a, b) => Number(b.recommended) - Number(a.recommended) || Number(a.status === "ineligible") - Number(b.status === "ineligible"))
                  .map((result) => (
                    <ResultCard key={result.id} result={result} />
                  ))}
              </ul>
            ) : (
              <p className="text-sm text-base-200">No U.S. or EU framework applies to these answers.</p>
            )}
            {assessment.notes.length > 0 && (
              <ul className="space-y-1 rounded-md border border-white/10 bg-base-900/60 px-3 py-2 text-xs text-base-200">
                {assessment.notes.map((note) => (
                  <li key={note}>{note}</li>
                ))}
              </ul>
            )}
            <div className="flex flex-wrap justify-between gap-3 border-t border-white/10 pt-4">
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => setStepIndex(steps.length - 1)}
                  className="rounded-md border border-white/10 px-4 py-2 text-sm text-base-100"
                >
                  Back
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setAnswers(EMPTY_ANSWERS);
                    setStepIndex(0);
                  }}
                  className="rounded-md border border-white/10 px-4 py-2 text-sm text-base-100"
                >
                  Start over
                </button>
              </div>
              <button
                type="button"
                onClick={exportJson}
                className="rounded-md border border-brand-500/40 bg-brand-500/10 px-4 py-2 text-sm font-semibold text-brand-300 hover:bg-brand-500/20"
              >
                Export JSON
              </button>
            </div>
          </div>
        )}
      </div>
    </section>
  );
}

export default ExemptionChecker;
