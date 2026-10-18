"use strict";

/**
 * Core of the MDX machine-translation workflow (issue #242), shared by
 * `scripts/translate-docs.mjs` and the Jest suite.
 *
 * Translation never sees code: a document is split into protected parts
 * (fenced code, `import`/`export` statements, MDX comments, JSX tags and
 * `{expressions}`, inline code, link targets, URLs and glossary terms) and
 * prose. Protected parts are replaced by numbered placeholders (`⟦0⟧`) before a
 * segment is sent to the provider and restored afterwards; a segment whose
 * placeholders do not come back exactly once is rejected. `validateParity`
 * then compares the source and translated files structurally (JSX tag
 * sequence, code blocks, inline code, link targets, imports, headings).
 */

const crypto = require("node:crypto");

const PLACEHOLDER_RE = /⟦(\d+)⟧/g;
const placeholder = (index) => `⟦${index}⟧`;

/** Product and protocol names kept in English in every locale. */
const DEFAULT_GLOSSARY = Object.freeze([
  "Tessera",
  "Stellar",
  "Soroban",
  "Horizon",
  "Freighter",
  "Stellar RWA Toolkit",
  "RWA",
  "KYC",
  "AML",
  "TVL",
  "SDK",
  "API",
  "REST",
  "GraphQL",
  "WebSocket",
  "JSON",
  "CSV",
  "XDR",
  "WASM",
  "Rust",
  "Axum",
  "Next.js",
  "TypeScript",
  "Gini",
]);

/** JSX attributes whose string values are user-facing text. */
const TRANSLATABLE_ATTRIBUTES = ["title", "alt", "label", "caption", "description", "aria-label", "placeholder"];

const LOCALE_NAMES = Object.freeze({
  en: "English",
  es: "Spanish",
  pt: "Portuguese (Brazil)",
  ja: "Japanese",
  fr: "French",
});

// ── Document segmentation ───────────────────────────────────────────────────

/** Index just past a `{...}` expression starting at `start`, honouring strings and nesting. */
function scanBraces(text, start) {
  let depth = 0;
  let quote = null;
  for (let i = start; i < text.length; i++) {
    const ch = text[i];
    if (quote) {
      if (ch === "\\") i++;
      else if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === "`") quote = ch;
    else if (ch === "{") depth++;
    else if (ch === "}") {
      depth--;
      if (depth === 0) return i + 1;
    }
  }
  return -1;
}

/** Index just past a JSX tag starting at `start` (`<`), or -1 if it is not a tag. */
function scanTag(text, start) {
  const next = text[start + 1];
  if (!(next === "/" || next === ">" || /[A-Za-z]/.test(next ?? ""))) return -1;
  let quote = null;
  for (let i = start + 1; i < text.length; i++) {
    const ch = text[i];
    if (quote) {
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") quote = ch;
    else if (ch === "{") {
      const end = scanBraces(text, i);
      if (end === -1) return -1;
      i = end - 1;
    } else if (ch === "<") return -1;
    else if (ch === ">") return i + 1;
  }
  return -1;
}

/** Index just past a balanced `(...)` starting at `start`. */
function scanParens(text, start) {
  let depth = 0;
  for (let i = start; i < text.length; i++) {
    if (text[i] === "(") depth++;
    else if (text[i] === ")") {
      depth--;
      if (depth === 0) return i + 1;
    } else if (text[i] === "\n" && depth > 0 && text[i + 1] === "\n") return -1;
  }
  return -1;
}

/**
 * Splits a JSX tag into protected markup and translatable attribute values,
 * e.g. `<CalloutBox title="Why this matters">` → markup, "Why this matters", markup.
 */
function splitTag(tag) {
  const parts = [];
  const re = new RegExp(`(\\s(?:${TRANSLATABLE_ATTRIBUTES.join("|")})=)("([^"{}]*)"|'([^'{}]*)')`, "g");
  let last = 0;
  let match;
  while ((match = re.exec(tag))) {
    const value = match[3] ?? match[4];
    const valueStart = match.index + match[1].length + 1;
    parts.push({ protected: true, text: tag.slice(last, valueStart) });
    if (/[A-Za-zÀ-ÿ]/.test(value)) parts.push({ protected: false, text: value });
    else parts.push({ protected: true, text: value });
    last = valueStart + value.length;
  }
  parts.push({ protected: true, text: tag.slice(last) });
  return parts;
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Splits plain text further so glossary terms stay untranslated. */
function protectGlossary(text, glossary) {
  if (!glossary.length || !text) return [{ protected: false, text }];
  const terms = [...glossary].sort((a, b) => b.length - a.length).map(escapeRegExp);
  const re = new RegExp(`(?<![\\p{L}\\p{N}_-])(?:${terms.join("|")})(?![\\p{L}\\p{N}_])`, "gu");
  const parts = [];
  let last = 0;
  let match;
  while ((match = re.exec(text))) {
    if (match.index > last) parts.push({ protected: false, text: text.slice(last, match.index) });
    parts.push({ protected: true, text: match[0] });
    last = match.index + match[0].length;
  }
  if (last < text.length) parts.push({ protected: false, text: text.slice(last) });
  return parts;
}

/**
 * Tokenizes a prose block into protected and translatable parts. Protected:
 * inline code, JSX tags (minus translatable attribute values), `{expressions}`,
 * link targets, raw URLs, HTML entities, table separator rows and glossary terms.
 */
function tokenizeProse(text, glossary) {
  const parts = [];
  let buffer = "";
  const flush = () => {
    if (buffer) parts.push(...protectGlossary(buffer, glossary));
    buffer = "";
  };
  const protect = (chunk) => {
    flush();
    parts.push({ protected: true, text: chunk });
  };

  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    const lineStart = i === 0 || text[i - 1] === "\n";

    if (lineStart) {
      const lineEnd = text.indexOf("\n", i) === -1 ? text.length : text.indexOf("\n", i);
      const line = text.slice(i, lineEnd);
      if (/^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$/.test(line) || /^\s*\[[^\]]+\]:\s*\S+/.test(line)) {
        protect(line);
        i = lineEnd;
        continue;
      }
    }

    if (ch === "`") {
      const run = /^`+/.exec(text.slice(i))[0];
      const close = text.indexOf(run, i + run.length);
      if (close !== -1) {
        protect(text.slice(i, close + run.length));
        i = close + run.length;
        continue;
      }
    }
    if (ch === "<") {
      const end = scanTag(text, i);
      if (end !== -1) {
        flush();
        parts.push(...splitTag(text.slice(i, end)));
        i = end;
        continue;
      }
    }
    if (ch === "{") {
      const end = scanBraces(text, i);
      if (end !== -1) {
        protect(text.slice(i, end));
        i = end;
        continue;
      }
    }
    if (ch === "]" && text[i + 1] === "(") {
      const end = scanParens(text, i + 1);
      if (end !== -1) {
        buffer += "]";
        protect(text.slice(i + 1, end));
        i = end;
        continue;
      }
    }
    if (ch === "h" && /^https?:\/\//.test(text.slice(i, i + 8))) {
      const match = /^https?:\/\/[^\s<>()\]]+/.exec(text.slice(i));
      protect(match[0]);
      i += match[0].length;
      continue;
    }
    if (ch === "&") {
      const match = /^&(?:[a-zA-Z]+|#\d+|#x[0-9a-fA-F]+);/.exec(text.slice(i));
      if (match) {
        protect(match[0]);
        i += match[0].length;
        continue;
      }
    }
    buffer += ch;
    i++;
  }
  flush();
  return parts;
}

/** Collapses parts into a segment: a template with placeholders plus the protected originals. */
function toSegment(parts) {
  const tokens = [];
  let template = "";
  let pending = null;
  const flushPending = () => {
    if (pending !== null) {
      template += placeholder(tokens.length);
      tokens.push(pending);
      pending = null;
    }
  };
  for (const part of parts) {
    if (part.protected) {
      pending = (pending ?? "") + part.text;
    } else if (part.text) {
      // Pure whitespace between protected parts stays inside the placeholder.
      if (pending !== null && /^\s*$/.test(part.text)) {
        pending += part.text;
        continue;
      }
      flushPending();
      template += part.text;
    }
  }
  flushPending();
  const translatable = /[\p{L}]/u.test(template.replace(PLACEHOLDER_RE, ""));
  return { template, tokens, translatable };
}

/** Restores placeholders; throws when any is missing, duplicated or invented. */
function restoreSegment(segment, translated) {
  const seen = new Map();
  for (const match of translated.matchAll(PLACEHOLDER_RE)) {
    const index = Number(match[1]);
    seen.set(index, (seen.get(index) ?? 0) + 1);
  }
  const problems = [];
  segment.tokens.forEach((_, index) => {
    const count = seen.get(index) ?? 0;
    if (count !== 1) problems.push(`${placeholder(index)} appears ${count} times`);
  });
  for (const index of seen.keys()) {
    if (index >= segment.tokens.length) problems.push(`unknown placeholder ${placeholder(index)}`);
  }
  if (problems.length) {
    const error = new Error(`Placeholder mismatch: ${problems.join(", ")}`);
    error.code = "PLACEHOLDER_MISMATCH";
    throw error;
  }
  return translated.replace(PLACEHOLDER_RE, (_, index) => segment.tokens[Number(index)]);
}

/** Index of the line that closes a `{ ... }` / statement starting at `start`. */
function statementEnd(lines, start) {
  let depth = 0;
  let quote = null;
  for (let l = start; l < lines.length; l++) {
    const line = lines[l];
    for (let c = 0; c < line.length; c++) {
      const ch = line[c];
      if (quote) {
        if (ch === "\\") c++;
        else if (ch === quote) quote = null;
        continue;
      }
      if (ch === '"' || ch === "'" || ch === "`") quote = ch;
      else if (ch === "{" || ch === "[" || ch === "(") depth++;
      else if (ch === "}" || ch === "]" || ch === ")") depth--;
    }
    if (depth <= 0 && quote === null) return l;
  }
  return lines.length - 1;
}

/**
 * Splits an MDX document into blocks: `protected` (copied verbatim), `metadata`
 * (the `export const metadata` object, whose `title` / `description` strings
 * are translated) and `prose`.
 */
function parseBlocks(source) {
  const lines = source.split("\n");
  const blocks = [];
  let prose = [];
  const flushProse = () => {
    if (prose.length) blocks.push({ type: "prose", text: prose.join("\n") });
    prose = [];
  };
  const push = (type, from, to) => {
    flushProse();
    blocks.push({ type, text: lines.slice(from, to + 1).join("\n") });
  };

  for (let l = 0; l < lines.length; l++) {
    const line = lines[l];
    const fence = /^(\s*)(`{3,}|~{3,})/.exec(line);
    if (fence) {
      const marker = fence[2];
      let end = l + 1;
      const closer = new RegExp(`^\\s*${marker[0] === "`" ? "`" : "~"}{${marker.length},}\\s*$`);
      while (end < lines.length && !closer.test(lines[end])) end++;
      push("protected", l, Math.min(end, lines.length - 1));
      l = end;
    } else if (/^export\s+const\s+metadata\s*=/.test(line)) {
      const end = statementEnd(lines, l);
      push("metadata", l, end);
      l = end;
    } else if (/^(import|export)\s/.test(line)) {
      const end = statementEnd(lines, l);
      push("protected", l, end);
      l = end;
    } else if (/^\s*\{\/\*/.test(line)) {
      let end = l;
      while (end < lines.length && !lines[end].includes("*/}")) end++;
      push("protected", l, Math.min(end, lines.length - 1));
      l = end;
    } else if (line.trim() === "") {
      push("protected", l, l);
    } else {
      prose.push(line);
    }
  }
  flushProse();
  return blocks;
}

const METADATA_STRING_RE = /(\b(?:title|description)\s*:\s*)("((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)')/g;

/** Segments for `title` / `description` in the metadata export. */
function metadataSegments(text, glossary) {
  const segments = [];
  for (const match of text.matchAll(METADATA_STRING_RE)) {
    const raw = match[3] ?? match[4];
    const value = raw.replace(/\\(["'\\])/g, "$1");
    segments.push({ ...toSegment(tokenizeProse(value, glossary)), match });
  }
  return segments;
}

function rebuildMetadata(text, segments, translations) {
  let index = 0;
  return text.replace(METADATA_STRING_RE, (whole, key, quoted) => {
    const segment = segments[index];
    const translated = translations[index++];
    if (!segment || translated === undefined) return whole;
    const q = quoted[0];
    return `${key}${q}${translated.replace(/\\/g, "\\\\").replace(new RegExp(q, "g"), `\\${q}`)}${q}`;
  });
}

/**
 * Translates an MDX document. `translateBatch(templates, locale)` receives
 * placeholder templates and must resolve to translations in the same order.
 * Segments are sent in chunks of at most `maxBatchChars` characters.
 */
async function translateDocument(source, options) {
  const { locale, translateBatch, glossary = DEFAULT_GLOSSARY, maxBatchChars = 6000, retries = 1 } = options;
  const blocks = parseBlocks(source);
  const plan = blocks.map((block) => {
    if (block.type === "prose") return { block, segments: [toSegment(tokenizeProse(block.text, glossary))] };
    if (block.type === "metadata") return { block, segments: metadataSegments(block.text, glossary) };
    return { block, segments: [] };
  });

  const pending = plan.flatMap((p) => p.segments.filter((s) => s.translatable));
  const results = new Map();
  for (let start = 0; start < pending.length; ) {
    let end = start;
    let size = 0;
    while (end < pending.length && (end === start || size + pending[end].template.length <= maxBatchChars)) {
      size += pending[end].template.length;
      end++;
    }
    const batch = pending.slice(start, end);
    for (let attempt = 0; ; attempt++) {
      const translations = await translateBatch(
        batch.map((s) => s.template),
        locale
      );
      if (!Array.isArray(translations) || translations.length !== batch.length) {
        throw new Error(`Provider returned ${Array.isArray(translations) ? translations.length : typeof translations} items for ${batch.length} segments`);
      }
      try {
        batch.forEach((segment, i) => results.set(segment, restoreSegment(segment, String(translations[i]))));
        break;
      } catch (error) {
        if (attempt >= retries) throw error;
      }
    }
    start = end;
  }

  return plan
    .map(({ block, segments }) => {
      if (block.type === "prose") {
        const [segment] = segments;
        return segment.translatable ? results.get(segment) : block.text;
      }
      if (block.type === "metadata") {
        return rebuildMetadata(
          block.text,
          segments,
          segments.map((s) => (s.translatable ? results.get(s) : restoreSegment(s, s.template)))
        );
      }
      return block.text;
    })
    .join("\n");
}

// ── Parity validation ───────────────────────────────────────────────────────

/** Removes fenced code, so tag/link scanning only sees prose and markup. */
function stripFences(text) {
  return parseBlocks(text)
    .filter((b) => !(b.type === "protected" && /^\s*(`{3,}|~{3,})/.test(b.text)))
    .map((b) => b.text)
    .join("\n");
}

/** Sequence of JSX tags (`<Name>`, `</Name>`, `<Name />`) outside code. */
function extractJsxTags(text) {
  const prose = stripFences(text).replace(/(`+)[\s\S]*?\1/g, "");
  const tags = [];
  for (let i = 0; i < prose.length; i++) {
    if (prose[i] !== "<") continue;
    const end = scanTag(prose, i);
    if (end === -1) continue;
    const tag = prose.slice(i, end);
    const name = /^<\/?\s*([A-Za-z][\w.:-]*)?/.exec(tag)?.[1] ?? "";
    if (/^<https?:/.test(tag)) {
      i = end - 1;
      continue;
    }
    const kind = tag.startsWith("</") ? "close" : /\/\s*>$/.test(tag) ? "self" : "open";
    tags.push(`${kind}:${name}`);
    i = end - 1;
  }
  return tags;
}

function fencedBlocks(text) {
  return parseBlocks(text)
    .filter((b) => b.type === "protected" && /^\s*(`{3,}|~{3,})/.test(b.text))
    .map((b) => b.text);
}

function inlineCode(text) {
  return (stripFences(text).match(/(`+)(?!`)[\s\S]*?\1/g) ?? []).sort();
}

function linkTargets(text) {
  return [...stripFences(text).matchAll(/\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g)].map((m) => m[1]);
}

function esmStatements(text) {
  return parseBlocks(text)
    .filter((b) => b.type === "protected" && /^(import|export)\s/.test(b.text))
    .map((b) => b.text.trim());
}

function headingLevels(text) {
  return (stripFences(text).match(/^#{1,6}(?=\s)/gm) ?? []).map((h) => h.length);
}

function firstDifference(a, b) {
  const length = Math.max(a.length, b.length);
  for (let i = 0; i < length; i++) {
    if (a[i] !== b[i]) return { index: i, expected: a[i] ?? "(none)", actual: b[i] ?? "(none)" };
  }
  return null;
}

/**
 * Compares a translation with its source. Returns human-readable errors; an
 * empty array means the translation is structurally faithful.
 */
function validateParity(source, target) {
  const errors = [];
  const compare = (label, a, b) => {
    const diff = firstDifference(a, b);
    if (diff) {
      errors.push(`${label} differ at #${diff.index + 1}: expected ${JSON.stringify(diff.expected)}, found ${JSON.stringify(diff.actual)}`);
    }
  };
  compare("JSX tags", extractJsxTags(source), extractJsxTags(target));
  compare("Code blocks", fencedBlocks(source), fencedBlocks(target));
  compare("Inline code spans", inlineCode(source), inlineCode(target));
  compare("Link targets", linkTargets(source), linkTargets(target));
  compare("import/export statements", esmStatements(source), esmStatements(target));
  compare("Heading levels", headingLevels(source), headingLevels(target));
  if (/⟦\d+⟧/.test(target)) errors.push("Unrestored placeholder found in translation");
  return errors;
}

// ── Provenance & routing ────────────────────────────────────────────────────

function sourceHash(source) {
  return crypto.createHash("sha256").update(source).digest("hex").slice(0, 16);
}

const HEADER_RE = /^\{\/\* translate-docs: (\{.*?\}) \*\/\}\n/;

/** Prepends an MDX comment recording the source file and hash the translation came from. */
function withProvenance(translated, info) {
  const meta = JSON.stringify({ source: info.source, sha: info.sha, locale: info.locale, provider: info.provider });
  return `{/* translate-docs: ${meta} */}\n${translated}`;
}

/** Reads the provenance header, or `null` for hand-written translations. */
function readProvenance(text) {
  const match = HEADER_RE.exec(text);
  if (!match) return null;
  try {
    return JSON.parse(match[1]);
  } catch {
    return null;
  }
}

function stripProvenance(text) {
  return text.replace(HEADER_RE, "");
}

/**
 * `page.tsx` that serves the translated MDX for the requested `[lang]` and
 * falls back to the English source (`page.en.mdx`).
 */
function renderLocalePage(locales) {
  const all = ["en", ...locales.filter((l) => l !== "en").sort()];
  const imports = all.map((l) => `import * as ${l} from "./page.${l}.mdx";`).join("\n");
  return `// Generated by scripts/translate-docs.mjs. Do not edit by hand.
// Serves the translated MDX for the requested locale; falls back to English (page.en.mdx).
import type { ComponentType } from "react";
import type { Metadata } from "next";
${imports}

interface MdxPage {
  default: ComponentType;
  metadata?: Metadata;
}

const PAGES = { ${all.join(", ")} } as unknown as Record<string, MdxPage>;

interface LocalizedPageProps {
  params: Promise<{ lang: string }>;
}

function pageFor(lang: string): MdxPage {
  return PAGES[lang] ?? PAGES.en;
}

export async function generateMetadata({ params }: LocalizedPageProps): Promise<Metadata> {
  const { lang } = await params;
  return pageFor(lang).metadata ?? PAGES.en.metadata ?? {};
}

export default async function LocalizedPage({ params }: LocalizedPageProps) {
  const { lang } = await params;
  const Content = pageFor(lang).default;
  return <Content />;
}
`;
}

const GENERATED_PAGE_MARKER = "// Generated by scripts/translate-docs.mjs.";

// ── Providers ───────────────────────────────────────────────────────────────

function systemPrompt(locale, glossary) {
  const language = LOCALE_NAMES[locale] ?? locale;
  return [
    `You translate technical documentation for Tessera, a real-world asset tokenization platform on Stellar, from English into ${language} (${locale}).`,
    "You receive a JSON array of strings. Translate each string and reply with ONLY a JSON array of the translated strings, same length and order.",
    "Rules:",
    "- Tokens like ⟦0⟧ stand for code, markup or links. Copy every token exactly once, unchanged, and keep it in a grammatically sensible position.",
    "- Keep Markdown syntax intact: heading markers, list markers, emphasis, table pipes and line breaks.",
    `- Do not translate these terms: ${glossary.join(", ")}.`,
    "- Use the formal register and established financial terminology of the target language.",
  ].join("\n");
}

function parseJsonArray(text) {
  const start = text.indexOf("[");
  const end = text.lastIndexOf("]");
  if (start === -1 || end <= start) throw new Error("Provider response did not contain a JSON array");
  const parsed = JSON.parse(text.slice(start, end + 1));
  if (!Array.isArray(parsed) || parsed.some((item) => typeof item !== "string")) {
    throw new Error("Provider response was not an array of strings");
  }
  return parsed;
}

const ACCENTS = { a: "á", e: "é", i: "í", o: "ö", u: "ü", A: "Á", E: "É", I: "Í", O: "Ö", U: "Ü" };

/** Deterministic pseudo-translation: accents vowels, keeps placeholders. For dry runs and tests. */
function pseudoTranslate(template, locale) {
  const body = template.replace(/⟦\d+⟧|[aeiouAEIOU]/g, (m) => ACCENTS[m] ?? m);
  // Tag after leading Markdown markers so headings and list items keep their syntax.
  const lead = /^\s*(?:(?:#{1,6}|[-*+]|\d+\.|>)\s+)*/.exec(body)[0];
  return `${lead}[${locale}] ${body.slice(lead.length)}`;
}

/**
 * Creates `translateBatch` for a provider:
 *  - `anthropic` — Claude Messages API (`ANTHROPIC_API_KEY`)
 *  - `openai`    — any OpenAI-compatible Chat Completions endpoint (`OPENAI_API_KEY`, `OPENAI_BASE_URL`)
 *  - `pseudo`    — offline pseudo-translation
 */
function createProvider(options) {
  const { name, model, apiKey, baseUrl, glossary = DEFAULT_GLOSSARY, fetchImpl = globalThis.fetch } = options;
  if (name === "pseudo") {
    return async (templates, locale) => templates.map((t) => pseudoTranslate(t, locale));
  }
  if (name !== "anthropic" && name !== "openai") {
    throw new Error(`Unknown provider "${name}" (expected anthropic, openai or pseudo)`);
  }
  if (!apiKey) throw new Error(`Provider "${name}" needs an API key`);
  if (typeof fetchImpl !== "function") throw new Error("global fetch is unavailable; use Node.js 18+");

  const call = async (url, init) => {
    const response = await fetchImpl(url, init);
    if (!response.ok) {
      const body = await response.text().catch(() => "");
      throw new Error(`${name} request failed: HTTP ${response.status} ${body.slice(0, 300)}`);
    }
    return response.json();
  };

  if (name === "anthropic") {
    return async (templates, locale) => {
      const data = await call(`${baseUrl ?? "https://api.anthropic.com"}/v1/messages`, {
        method: "POST",
        headers: { "content-type": "application/json", "x-api-key": apiKey, "anthropic-version": "2023-06-01" },
        body: JSON.stringify({
          model,
          max_tokens: 8192,
          temperature: 0,
          system: systemPrompt(locale, glossary),
          messages: [{ role: "user", content: JSON.stringify(templates) }],
        }),
      });
      const text = (data.content ?? []).filter((part) => part.type === "text").map((part) => part.text).join("");
      return parseJsonArray(text);
    };
  }
  // name === "openai": any OpenAI-compatible Chat Completions endpoint.
  return async (templates, locale) => {
    const data = await call(`${(baseUrl ?? "https://api.openai.com/v1").replace(/\/+$/, "")}/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${apiKey}` },
      body: JSON.stringify({
        model,
        temperature: 0,
        messages: [
          { role: "system", content: systemPrompt(locale, glossary) },
          { role: "user", content: JSON.stringify(templates) },
        ],
      }),
    });
    return parseJsonArray(data.choices?.[0]?.message?.content ?? "");
  };
}

module.exports = {
  DEFAULT_GLOSSARY,
  LOCALE_NAMES,
  GENERATED_PAGE_MARKER,
  parseBlocks,
  tokenizeProse,
  toSegment,
  restoreSegment,
  translateDocument,
  validateParity,
  extractJsxTags,
  sourceHash,
  withProvenance,
  readProvenance,
  stripProvenance,
  renderLocalePage,
  createProvider,
  pseudoTranslate,
  parseJsonArray,
  systemPrompt,
};
