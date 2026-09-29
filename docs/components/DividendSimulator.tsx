"use client";

import { useState } from "react";
import { Bar, BarChart, CartesianGrid, Legend, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";

const TAX_RATES: Record<string, number> = { US: 15, GB: 20, DE: 26.375, FR: 25, SG: 10, Other: 25 };

export default function DividendSimulator() {
  const [tokens, setTokens] = useState(1000);
  const [yieldPercent, setYieldPercent] = useState(6);
  const [jurisdiction, setJurisdiction] = useState("US");
  const [drip, setDrip] = useState(false);
  const taxRate = TAX_RATES[jurisdiction];
  const gross = tokens * yieldPercent;
  const tax = gross * (taxRate / 100);
  const net = gross - tax;
  const reinvested = net * (1 + yieldPercent / 100);
  const chartData = [
    { name: "Year 1", cash: net, drip: reinvested },
    { name: "Year 2", cash: net * 2, drip: reinvested * (1 + yieldPercent / 100) },
    { name: "Year 3", cash: net * 3, drip: reinvested * (1 + yieldPercent / 100) ** 2 },
    { name: "Year 4", cash: net * 4, drip: reinvested * (1 + yieldPercent / 100) ** 3 },
    { name: "Year 5", cash: net * 5, drip: reinvested * (1 + yieldPercent / 100) ** 4 },
  ];

  return (
    <section className="my-8 rounded-xl border border-white/10 bg-white/[0.03] p-5" aria-labelledby="dividend-simulator-title">
      <div className="mb-5 flex flex-wrap items-end justify-between gap-3">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.18em] text-brand-300">Investor planning</p>
          <h2 id="dividend-simulator-title" className="text-xl font-semibold text-base-100">Dividend and tax estimator</h2>
        </div>
        <label className="flex items-center gap-2 text-sm text-base-200">
          <input type="checkbox" checked={drip} onChange={(event) => setDrip(event.target.checked)} />
          Reinvest dividends (DRIP)
        </label>
      </div>
      <div className="grid gap-4 md:grid-cols-3">
        <label className="text-sm text-base-200">Token holding
          <input className="mt-1 w-full rounded-md border border-white/10 bg-black/20 px-3 py-2" type="number" min="0" value={tokens} onChange={(event) => setTokens(Number(event.target.value) || 0)} />
        </label>
        <label className="text-sm text-base-200">Projected annual rental yield (%)
          <input className="mt-1 w-full rounded-md border border-white/10 bg-black/20 px-3 py-2" type="number" min="0" step="0.1" value={yieldPercent} onChange={(event) => setYieldPercent(Number(event.target.value) || 0)} />
        </label>
        <label className="text-sm text-base-200">Investor jurisdiction
          <select className="mt-1 w-full rounded-md border border-white/10 bg-black/20 px-3 py-2" value={jurisdiction} onChange={(event) => setJurisdiction(event.target.value)}>
            {Object.keys(TAX_RATES).map((country) => <option key={country} value={country}>{country}</option>)}
          </select>
        </label>
      </div>
      <dl className="my-6 grid gap-3 sm:grid-cols-3">
        <div><dt className="text-xs text-base-400">Gross annual dividend</dt><dd className="text-2xl font-semibold text-base-100">${gross.toFixed(2)}</dd></div>
        <div><dt className="text-xs text-base-400">Withholding tax ({taxRate}%)</dt><dd className="text-2xl font-semibold text-amber-300">${tax.toFixed(2)}</dd></div>
        <div><dt className="text-xs text-base-400">Net annual payout</dt><dd className="text-2xl font-semibold text-emerald-300">${net.toFixed(2)}</dd></div>
      </dl>
      <div className="h-64 w-full" role="img" aria-label="Five-year dividend comparison with and without automatic reinvestment">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart data={chartData}>
            <CartesianGrid strokeDasharray="3 3" stroke="#334155" />
            <XAxis dataKey="name" stroke="#94a3b8" />
            <YAxis stroke="#94a3b8" />
            <Tooltip formatter={(value: number) => `$${value.toFixed(2)}`} />
            <Legend />
            <Bar dataKey="cash" name="Cash payout" fill="#38bdf8" />
            <Bar dataKey="drip" name="DRIP reinvested" fill={drip ? "#34d399" : "#64748b"} />
          </BarChart>
        </ResponsiveContainer>
      </div>
      <p className="mt-3 text-xs text-base-400">Estimates are illustrative and do not account for local filing obligations, fees, or changes in tax law.</p>
    </section>
  );
}