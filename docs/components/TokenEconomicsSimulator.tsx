"use client";

import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  curveMonotoneX,
  interpolateViridis,
  line,
  scaleLinear,
  scaleSequential,
} from "d3";
import {
  DEFAULT_PARAMS,
  parseScenario,
  TokenEconomicsEngine,
  type Scenario,
  type SimOutput,
  type SimParams,
} from "@/lib/token-economics-engine";

export interface TokenEconomicsSimulatorProps {
  /** Initial scenario parameters; sliders are seeded from these values. */
  initialParams?: Partial<SimParams>;
  /** Scenario name shown in the exported JSON and the header. */
  name?: string;
  /** Initial number of simulation steps (1-10000). */
  steps?: number;
  /** Extra CSS class on the root section. */
  className?: string;
}

type WorkerMessage =
  | { type: "progress"; value: number }
  | { type: "notice"; message: string }
  | { type: "result"; output: SimOutput; engine: "wasm" | "typescript" }
  | { type: "error"; message: string };

interface SliderSpec {
  key: keyof SimParams;
  label: string;
  min: number;
  max: number;
  step: number;
  hint: string;
}

const SLIDERS: SliderSpec[] = [
  { key: "steps", label: "Steps", min: 20, max: 4000, step: 20, hint: "Simulation horizon" },
  { key: "institutional", label: "Institutional holders", min: 0, max: 500, step: 5, hint: "Buy on dips" },
  { key: "retail", label: "Retail holders", min: 0, max: 2000, step: 10, hint: "Follow momentum" },
  { key: "arbitrageur", label: "Arbitrageurs", min: 0, max: 500, step: 5, hint: "Exploit AMM/oracle gaps" },
  { key: "dipThreshold", label: "Dip threshold", min: 0, max: 0.4, step: 0.005, hint: "Institutional buy trigger" },
  { key: "momentumWindow", label: "Momentum window", min: 1, max: 100, step: 1, hint: "Retail lookback (steps)" },
  { key: "arbitrageTolerance", label: "Arbitrage tolerance", min: 0, max: 0.05, step: 0.001, hint: "Minimum exploitable gap" },
  { key: "volatility", label: "Volatility", min: 0, max: 0.2, step: 0.001, hint: "Oracle price volatility" },
  { key: "liquidityDepth", label: "Liquidity depth", min: 100, max: 20000, step: 100, hint: "AMM order-book depth" },
  { key: "dividendRate", label: "Dividend rate", min: 0, max: 0.3, step: 0.005, hint: "Annualised payout" },
  { key: "dividendLambda", label: "Dividend interval", min: 2, max: 500, step: 1, hint: "Poisson mean gap (steps)" },
];

function LineChart({
  series,
  label,
  color,
  accent,
}: {
  series: number[];
  label: string;
  color: string;
  accent?: number[];
}) {
  const width = 620;
  const height = 200;
  const margin = { top: 12, right: 12, bottom: 24, left: 48 };
  if (series.length < 2) return <p className="text-sm text-base-400">No data yet.</p>;
  const all = accent ? [...series, ...accent] : series;
  const x = scaleLinear().domain([0, series.length - 1]).range([margin.left, width - margin.right]);
  const y = scaleLinear()
    .domain([Math.min(...all) * 0.98, Math.max(...all) * 1.02])
    .range([height - margin.bottom, margin.top]);
  const path = line<number>()
    .x((_, i) => x(i))
    .y((value) => y(value))
    .curve(curveMonotoneX)(series);
  const accentPath = accent
    ? line<number>().x((_, i) => x(i)).y((value) => y(value)).curve(curveMonotoneX)(accent)
    : undefined;

  return (
    <figure>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        className="w-full"
        role="img"
        aria-label={`${label} over time`}
      >
        <line x1={margin.left} x2={width - margin.right} y1={height - margin.bottom} y2={height - margin.bottom} stroke="#334155" />
        <line x1={margin.left} x2={margin.left} y1={margin.top} y2={height - margin.bottom} stroke="#334155" />
        <text x={margin.left} y={height - 4} fill="#94a3b8" fontSize="10">step 0</text>
        <text x={width - margin.right} y={height - 4} fill="#94a3b8" fontSize="10" textAnchor="end">step {series.length - 1}</text>
        {accentPath && <path d={accentPath} fill="none" stroke="#64748b" strokeDasharray="4 3" strokeWidth="1.5" />}
        <path d={path ?? ""} fill="none" stroke={color} strokeWidth="2" />
      </svg>
      <figcaption className="mt-1 text-xs text-base-400">
        {label}{accent ? " · dashed = oracle" : ""}
      </figcaption>
    </figure>
  );
}

function Heatmap({
  matrix,
  label,
  colorize,
}: {
  matrix: number[][];
  label: string;
  colorize?: boolean;
}) {
  if (!matrix.length) return <p className="text-sm text-base-400">No data yet.</p>;
  const cols = Math.max(...matrix.map((row) => row.length));
  const cell = 8;
  const width = cols * cell;
  const height = matrix.length * cell;
  const max = Math.max(1, ...matrix.flat());
  const color = scaleSequential(interpolateViridis).domain([0, max]);
  return (
    <figure>
      <svg
        viewBox={`0 0 ${Math.max(width, 1)} ${Math.max(height, 1)}`}
        className="max-h-48 w-full"
        role="img"
        aria-label={`${label} heatmap`}
      >
        {matrix.flatMap((row, t) =>
          row.map((value, bin) => (
            <rect
              key={`${t}-${bin}`}
              x={bin * cell}
              y={t * cell}
              width={cell - 1}
              height={cell - 1}
              fill={colorize ? color(value) : `rgba(56,189,248,${0.1 + (value / max) * 0.9})`}
            />
          )),
        )}
      </svg>
      <figcaption className="mt-1 text-xs text-base-400">{label} · {matrix.length} samples × {cols} buckets</figcaption>
    </figure>
  );
}

/** Interactive agent-based token-economics simulator, backed by Rust WASM. */
export default function TokenEconomicsSimulator({
  initialParams,
  name = "Sandbox token economy",
  steps,
  className = "",
}: TokenEconomicsSimulatorProps) {
  const generatedId = useId();
  const [params, setParams] = useState<SimParams>(() => ({
    ...DEFAULT_PARAMS,
    ...initialParams,
    ...(steps === undefined ? {} : { steps }),
  }));
  const [scenarioName, setScenarioName] = useState(name);
  const [output, setOutput] = useState<SimOutput | null>(null);
  const [progress, setProgress] = useState(0);
  const [engine, setEngine] = useState<"wasm" | "typescript" | "reference" | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [runId, setRunId] = useState(0);
  const fileInput = useRef<HTMLInputElement>(null);

  const scenario = useMemo<Scenario>(
    () => ({ version: 1, name: scenarioName, params }),
    [params, scenarioName],
  );

  useEffect(() => {
    setRunning(true);
    setProgress(0);
    setNotice(null);
    const handle = (message: WorkerMessage) => {
      switch (message.type) {
        case "progress":
          setProgress(message.value);
          break;
        case "notice":
          setNotice(message.message);
          break;
        case "result":
          setOutput(message.output);
          setEngine(message.engine);
          setProgress(1);
          setRunning(false);
          break;
        case "error":
          setNotice(message.message);
          setRunning(false);
          break;
      }
    };

    // jsdom / SSR has no Worker: run the reference model inline so the
    // component is testable without a browser.
    if (typeof Worker === "undefined") {
      const result = new TokenEconomicsEngine(scenario).run();
      setOutput(result);
      setEngine("reference");
      setProgress(1);
      setRunning(false);
      return;
    }

    const worker = new Worker("/token-economics-worker.js");
    worker.onmessage = (event: MessageEvent<WorkerMessage>) => handle(event.data);
    worker.onerror = () => {
      const result = new TokenEconomicsEngine(scenario).run();
      setOutput(result);
      setEngine("reference");
      setProgress(1);
      setRunning(false);
    };
    worker.postMessage({ scenario });
    return () => worker.terminate();
  }, [runId, scenario]);

  const exportScenario = () => {
    const blob = new Blob([JSON.stringify(scenario, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `${scenarioName.toLowerCase().replace(/[^a-z0-9]+/g, "-") || "scenario"}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  const importScenario = async (file: File | undefined) => {
    if (!file) return;
    try {
      const parsed = parseScenario(JSON.parse(await file.text()));
      setParams(parsed.params);
      setScenarioName(parsed.name);
      setNotice(`Imported scenario "${parsed.name}"`);
    } catch (error) {
      setNotice(`Import failed: ${error instanceof Error ? error.message : String(error)}`);
    }
  };

  const setParam = (key: keyof SimParams, value: number) =>
    setParams((current) => ({ ...current, [key]: value }));

  return (
    <section
      className={`my-8 rounded-xl border border-white/10 bg-white/[0.03] p-5 ${className}`}
      aria-labelledby={`${generatedId}-title`}
      data-testid="token-economics-simulator"
    >
      <div className="mb-5 flex flex-wrap items-end justify-between gap-3">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.18em] text-brand-300">Modelling</p>
          <h2 id={`${generatedId}-title`} className="text-xl font-semibold text-base-100">
            Token economics simulator
          </h2>
          <p className="mt-1 max-w-2xl text-sm text-base-300">
            Institutional holders buy dips, retail holders chase momentum, and
            arbitrageurs close the gap between the AMM and oracle price.
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => setRunId((value) => value + 1)}
            disabled={running}
            className="rounded-md bg-brand-500 px-4 py-2 text-sm font-semibold text-white disabled:opacity-50"
          >
            {running ? `Running ${Math.round(progress * 100)}%` : "Run simulation"}
          </button>
          <button type="button" onClick={exportScenario} className="rounded-md border border-white/15 px-3 py-2 text-sm">
            Export JSON
          </button>
          <button type="button" onClick={() => fileInput.current?.click()} className="rounded-md border border-white/15 px-3 py-2 text-sm">
            Import JSON
          </button>
          <input
            ref={fileInput}
            type="file"
            accept="application/json,.json"
            className="sr-only"
            aria-label="Import a scenario JSON file"
            onChange={(event) => void importScenario(event.target.files?.[0])}
          />
        </div>
      </div>

      <label className="mb-4 block text-sm text-base-200">
        Scenario name
        <input
          className="mt-1 w-full max-w-md rounded-md border border-white/10 bg-black/20 px-3 py-2"
          value={scenarioName}
          onChange={(event) => setScenarioName(event.target.value)}
        />
      </label>

      <div className="grid gap-x-6 gap-y-3 md:grid-cols-2 lg:grid-cols-3">
        {SLIDERS.map((spec) => (
          <label key={spec.key} className="text-sm text-base-200">
            {spec.label}
            <span className="ml-2 text-xs text-base-400">{spec.hint}</span>
            <input
              type="range"
              min={spec.min}
              max={spec.max}
              step={spec.step}
              value={params[spec.key]}
              onChange={(event) => setParam(spec.key, Number(event.target.value))}
              className="mt-1 w-full"
              aria-label={`${spec.label} (${spec.hint})`}
            />
            <output className="block text-xs text-brand-300">{params[spec.key]}</output>
          </label>
        ))}
      </div>

      <div className="mt-5" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(progress * 100)}>
        <div className="h-1.5 w-full overflow-hidden rounded bg-white/10">
          <div className="h-full bg-brand-400 transition-all" style={{ width: `${progress * 100}%` }} />
        </div>
      </div>
      {notice && <p className="mt-2 text-xs text-amber-300" role="status">{notice}</p>}
      {engine && <p className="mt-1 text-xs text-base-400" data-testid="simulator-engine">engine: {engine}</p>}

      <div className="mt-6 grid gap-6 lg:grid-cols-2">
        {output && (
          <>
            <LineChart
              series={output.price_series}
              accent={output.oracle_series}
              label="AMM price series"
              color="#38bdf8"
            />
            <LineChart series={output.dividend_yield} label="Dividend yield trajectory" color="#34d399" />
            <Heatmap matrix={output.holder_distribution} label="Holder distribution over time" />
            <Heatmap matrix={output.liquidity_depth} label="Liquidity depth" colorize />
          </>
        )}
      </div>
      {output && (
        <dl className="mt-5 grid gap-3 sm:grid-cols-3">
          <div><dt className="text-xs text-base-400">Final Gini</dt><dd className="text-xl font-semibold">{output.final_gini.toFixed(3)}</dd></div>
          <div><dt className="text-xs text-base-400">Max drawdown</dt><dd className="text-xl font-semibold">{(output.max_drawdown * 100).toFixed(1)}%</dd></div>
          <div><dt className="text-xs text-base-400">Steps</dt><dd className="text-xl font-semibold">{output.steps}</dd></div>
        </dl>
      )}
      <p className="mt-4 text-xs text-base-400">
        Illustrative agent-based model. Not investment advice; parameters are
        exported verbatim so results can be reproduced.
      </p>
    </section>
  );
}
