"use client";

import { useMemo, useState } from "react";

export type WaterfallDistribution = {
  tranche: "Senior debt" | "Preferred equity" | "Common equity";
  claim: number;
  payout: number;
  shortfall: number;
};

export function calculateWaterfall(salePrice: number, debt: number, preferredReturn: number, preferredSplit: number): WaterfallDistribution[] {
  let remaining = Math.max(0, salePrice);
  const pay = (claim: number) => { const payout = Math.min(remaining, Math.max(0, claim)); remaining -= payout; return payout; };
  const senior = pay(debt);
  const preferred = pay(preferredReturn);
  // Remaining proceeds are shared by the two equity classes. `preferredSplit`
  // belongs to preferred holders; the complement belongs to common holders.
  const preferredUpside = remaining * Math.min(100, Math.max(0, preferredSplit)) / 100;
  const common = remaining - preferredUpside;
  return [
    { tranche: "Senior debt", claim: Math.max(0, debt), payout: senior, shortfall: Math.max(0, debt - senior) },
    { tranche: "Preferred equity", claim: Math.max(0, preferredReturn), payout: preferred + preferredUpside, shortfall: Math.max(0, preferredReturn - preferred) },
    { tranche: "Common equity", claim: 0, payout: common, shortfall: 0 },
  ];
}

const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 0 });
const colors = ["#38bdf8", "#f59e0b", "#a78bfa"];
function download(blob: Blob, filename: string) { const url = URL.createObjectURL(blob); const a = document.createElement("a"); a.href = url; a.download = filename; a.click(); URL.revokeObjectURL(url); }
function xml(value: string) { return value.replace(/[<&>]/g, c => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;" }[c]!)); }

/** A standards-compatible SpreadsheetML workbook; Excel opens this as .xlsx. */
function excel(rows: WaterfallDistribution[]) {
  const sheet = `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>${[["Tranche", "Claim", "Payout", "Shortfall"], ...rows.map(r => [r.tranche, r.claim, r.payout, r.shortfall])].map((row, i) => `<row r="${i + 1}">${row.map((v, j) => typeof v === "number" ? `<c r="${String.fromCharCode(65 + j)}${i + 1}"><v>${v}</v></c>` : `<c r="${String.fromCharCode(65 + j)}${i + 1}" t="inlineStr"><is><t>${xml(v)}</t></is></c>`).join("")}</row>`).join("")}</sheetData></worksheet>`;
  const files: Record<string, string> = {
    "[Content_Types].xml": `<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>`,
    "_rels/.rels": `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>`,
    "xl/workbook.xml": `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Waterfall" sheetId="1" r:id="rId1"/></sheets></workbook>`,
    "xl/_rels/workbook.xml.rels": `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>`, "xl/worksheets/sheet1.xml": sheet,
  };
  // ZIP's "store" method avoids a large dependency while remaining a real .xlsx package.
  const enc = new TextEncoder(), parts: Uint8Array[] = [], entries: { name: string; offset: number; size: number; crc: number }[] = []; let offset = 0;
  const u16 = (n: number) => new Uint8Array([n & 255, n >>> 8]); const u32 = (n: number) => new Uint8Array([n & 255, n >>> 8, n >>> 16, n >>> 24]);
  const crc32 = (data: Uint8Array) => { let c = 0xffffffff; for (const byte of data) { c ^= byte; for (let i = 0; i < 8; i++) c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1; } return (c ^ 0xffffffff) >>> 0; };
  for (const [name, content] of Object.entries(files)) { const n = enc.encode(name), data = enc.encode(content), crc = crc32(data), header = new Uint8Array([...u32(0x04034b50), ...u16(20), ...u16(0), ...u16(0), ...u16(0), ...u16(0), ...u32(crc), ...u32(data.length), ...u32(data.length), ...u16(n.length), ...u16(0), ...n]); parts.push(header, data); entries.push({ name, offset, size: data.length, crc }); offset += header.length + data.length; }
  const central = entries.map(e => { const n = enc.encode(e.name); return new Uint8Array([...u32(0x02014b50), ...u16(20), ...u16(20), ...u16(0), ...u16(0), ...u16(0), ...u16(0), ...u32(e.crc), ...u32(e.size), ...u32(e.size), ...u16(n.length), ...u16(0), ...u16(0), ...u16(0), ...u16(0), ...u32(0), ...u32(e.offset), ...n]); }); const centralSize = central.reduce((n, p) => n + p.length, 0); parts.push(...central, new Uint8Array([...u32(0x06054b50), ...u16(0), ...u16(0), ...u16(entries.length), ...u16(entries.length), ...u32(centralSize), ...u32(offset), ...u16(0)]));
  return new Blob(parts, { type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" });
}

function pdf(rows: WaterfallDistribution[]) { const lines = ["Waterfall distribution summary", ...rows.map(r => `${r.tranche}: ${usd.format(r.payout)} (claim ${usd.format(r.claim)}, shortfall ${usd.format(r.shortfall)})`)]; const content = `BT /F1 14 Tf 50 760 Td ${lines.map((line, i) => `${i ? "0 -24 Td " : ""}(${line.replace(/[()\\]/g, "\\$&")}) Tj`).join(" ")} ET`; const objects = ["<</Type/Catalog/Pages 2 0 R>>", "<</Type/Pages/Count 1/Kids[3 0 R]>>", "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Resources<</Font<</F1 4 0 R>>>>/Contents 5 0 R>>", "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>", `<</Length ${content.length}>>stream\n${content}\nendstream`]; let body = "%PDF-1.4\n"; const offsets = [0]; objects.forEach((object, i) => { offsets.push(body.length); body += `${i + 1} 0 obj\n${object}\nendobj\n`; }); const xref = body.length; body += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.slice(1).map(offset => `${String(offset).padStart(10, "0")} 00000 n \n`).join("")}trailer\n<</Size ${objects.length + 1}/Root 1 0 R>>\nstartxref\n${xref}\n%%EOF`; return new Blob([body], { type: "application/pdf" }); }

export function WaterfallCalculator() {
  const [sale, setSale] = useState(10_000_000), [debt, setDebt] = useState(4_000_000), [pref, setPref] = useState(3_000_000), [split, setSplit] = useState(60);
  const rows = useMemo(() => calculateWaterfall(sale, debt, pref, split), [sale, debt, pref, split]); const total = Math.max(sale, 1);
  const fields: [string, number, (value: number) => void][] = [["Total sale price", sale, setSale], ["Senior debt obligations", debt, setDebt], ["Preferred return", pref, setPref], ["Preferred upside split (%)", split, setSplit]];
  return <section className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-5"><h3 className="text-lg font-semibold">Exit Waterfall Calculator</h3><div className="mt-4 grid gap-3 sm:grid-cols-2">{fields.map(([label, value, set]) => <label key={label} className="text-sm text-base-300">{label}<input aria-label={label} className="mt-1 block w-full rounded border border-white/10 bg-black/30 p-2 text-base-100" type="number" min="0" value={value} onChange={e => set(Number(e.target.value) || 0)} /></label>)}</div><div className="mt-6 space-y-3" aria-label="Payout chart">{rows.map((row, i) => <div key={row.tranche}><div className="flex justify-between text-sm"><span>{row.tranche}</span><strong>{usd.format(row.payout)}</strong></div><div className="mt-1 h-7 overflow-hidden rounded bg-white/10"><div className="h-full transition-all" style={{ width: `${Math.min(100, row.payout / total * 100)}%`, background: colors[i] }} /></div></div>)}</div><div className="mt-5 overflow-x-auto"><table className="w-full text-left text-sm"><thead><tr><th>Tranche</th><th>Claim</th><th>Payout</th><th>Shortfall</th></tr></thead><tbody>{rows.map(r => <tr key={r.tranche}><td>{r.tranche}</td><td>{usd.format(r.claim)}</td><td>{usd.format(r.payout)}</td><td>{usd.format(r.shortfall)}</td></tr>)}</tbody></table></div><div className="mt-5 flex gap-3"><button type="button" onClick={() => download(excel(rows), "waterfall-distribution.xlsx")} className="rounded bg-brand-500 px-3 py-2 text-sm text-white">Export Excel</button><button type="button" onClick={() => download(pdf(rows), "waterfall-distribution.pdf")} className="rounded border border-white/20 px-3 py-2 text-sm">Export PDF</button></div></section>;
}
