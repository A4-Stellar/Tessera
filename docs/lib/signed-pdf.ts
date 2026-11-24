/**
 * Minimal PDF 1.4 writer for signed cap-table reports (issue #476).
 *
 * The PDF has the human-readable report on its pages, a signature block, and
 * the exact signed JSON attached as `report.json` (an embedded file). To
 * verify a report, extract the attachment, SHA-256 its bytes and check the
 * Ed25519 signature over that digest with the printed public key. No
 * dependencies: Helvetica, ASCII text, uncompressed streams.
 */

export interface PdfSignatureBlock {
  algorithm: string;
  digest: string;
  signature: string;
  publicKey: string;
  signedAt: string;
}

export interface SignedPdfInput {
  title: string;
  /** Body lines, already formatted. Long lines are wrapped. */
  lines: string[];
  signature: PdfSignatureBlock;
  /** Exactly the bytes that were signed (canonical JSON). */
  signedJson: string;
}

const PAGE_W = 612;
const PAGE_H = 792;
const MARGIN = 50;
const FONT_SIZE = 9;
const LEADING = 12;
const MAX_CHARS = 105;
const LINES_PER_PAGE = Math.floor((PAGE_H - 2 * MARGIN - 30) / LEADING);

/** Printable ASCII only; PDF string delimiters escaped. */
function pdfText(s: string): string {
  return s
    .normalize("NFKD")
    .replace(/[^\x20-\x7e]/g, "?")
    .replace(/\\/g, "\\\\")
    .replace(/\(/g, "\\(")
    .replace(/\)/g, "\\)");
}

function wrap(line: string): string[] {
  if (line.length <= MAX_CHARS) return [line];
  const out: string[] = [];
  for (let i = 0; i < line.length; i += MAX_CHARS) out.push(line.slice(i, i + MAX_CHARS));
  return out;
}

/** UTF-8 bytes of a string, as a binary (latin1) string for the PDF body. */
function utf8Binary(s: string): string {
  return Array.from(new TextEncoder().encode(s), (b) => String.fromCharCode(b)).join("");
}

export function buildSignedPdf(input: SignedPdfInput): Uint8Array<ArrayBuffer> {
  const sig = input.signature;
  const body = [
    ...input.lines,
    "",
    "Signature",
    `  algorithm   ${sig.algorithm}`,
    `  signed at   ${sig.signedAt}`,
    `  sha-256     ${sig.digest}`,
    `  public key  ${sig.publicKey}`,
    `  signature   ${sig.signature.slice(0, 64)}`,
    `              ${sig.signature.slice(64)}`,
    "",
    "Verify: extract the attached report.json, SHA-256 its bytes, compare with the digest above,",
    "and check the Ed25519 signature of that digest with the public key.",
  ].flatMap(wrap);

  const pages: string[][] = [];
  for (let i = 0; i < body.length; i += LINES_PER_PAGE) pages.push(body.slice(i, i + LINES_PER_PAGE));
  if (pages.length === 0) pages.push([]);

  // Object numbers: 1 catalog, 2 pages, 3 font, 4 embedded file, 5 filespec,
  // then a (page, content) pair per page.
  const objects: string[] = [];
  const pageIds = pages.map((_, i) => 6 + i * 2);
  const json = utf8Binary(input.signedJson);

  objects[1] = `<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(report.json) 5 0 R] >> >> /PageMode /UseAttachments >>`;
  objects[2] = `<< /Type /Pages /Kids [${pageIds.map((id) => `${id} 0 R`).join(" ")}] /Count ${pages.length} >>`;
  objects[3] = `<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>`;
  objects[4] = `<< /Type /EmbeddedFile /Subtype /application#2Fjson /Length ${json.length} >>\nstream\n${json}\nendstream`;
  objects[5] = `<< /Type /Filespec /F (report.json) /UF (report.json) /EF << /F 4 0 R >> /Desc (Signed report payload) >>`;

  pages.forEach((lines, i) => {
    const pageId = pageIds[i];
    const contentId = pageId + 1;
    const header = `${pdfText(input.title)}  -  page ${i + 1} of ${pages.length}`;
    const ops = [
      "BT",
      `/F1 12 Tf`,
      `${MARGIN} ${PAGE_H - MARGIN} Td`,
      `(${header}) Tj`,
      `/F1 ${FONT_SIZE} Tf`,
      `${LEADING} TL`,
      `0 -${LEADING * 2} Td`,
      ...lines.map((l) => `(${pdfText(l)}) '`),
      "ET",
    ].join("\n");
    objects[pageId] = `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${PAGE_W} ${PAGE_H}] /Resources << /Font << /F1 3 0 R >> >> /Contents ${contentId} 0 R >>`;
    objects[contentId] = `<< /Length ${ops.length} >>\nstream\n${ops}\nendstream`;
  });

  let out = "%PDF-1.4\n%\xe2\xe3\xcf\xd3\n";
  const offsets: number[] = [];
  for (let id = 1; id < objects.length; id++) {
    offsets[id] = out.length;
    out += `${id} 0 obj\n${objects[id]}\nendobj\n`;
  }
  const xref = out.length;
  out += `xref\n0 ${objects.length}\n0000000000 65535 f \n`;
  for (let id = 1; id < objects.length; id++) out += `${String(offsets[id]).padStart(10, "0")} 00000 n \n`;
  out += `trailer\n<< /Size ${objects.length} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;

  // Every character is a single byte (latin1), so lengths above are byte counts.
  return Uint8Array.from(out, (c) => c.charCodeAt(0) & 0xff);
}

/**
 * Canonical JSON as the API computes it: object keys sorted, no whitespace.
 * The API signs `serde_json` output of a re-parsed value, which sorts keys
 * and prints numbers the same way as `JSON.stringify` for these reports.
 */
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, v]) => v !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonicalJson(v)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
