"use strict";

/**
 * Core of the MDX machine-translation workflow (issues #242 & #442), shared by
 * `scripts/translate-docs.mjs` and the Jest suite.
 *
 * Translation never sees code: a document is split into protected parts
 * (fenced code, `{/* i18n-freeze *\/}` blocks, `import`/`export` statements,
 * MDX comments, JSX tags and `{expressions}`, inline code, link targets, URLs,
 * Stellar contract/account addresses, and glossary terms) and prose. Protected
 * parts are replaced by numbered placeholders (`⟦0⟧`) before a segment is sent
 * to the provider and restored afterwards; a segment whose placeholders do not
 * come back exactly once is rejected. `validateParity` then compares the source
 * and translated files structurally (JSX tag sequence, code blocks, inline code,
 * link targets, imports, headings, frozen blocks, and contract addresses).
 */

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const PLACEHOLDER_RE = /⟦(\d+)⟧/g;
const placeholder = (index) => `⟦${index}⟧`;

/** Stellar StrKey contract (`C...`) and account (`G...`) 56-character base32 addresses. */
const STELLAR_ADDRESS_RE = /\b[CG][A-Z2-7]{55}\b/g;

/** Inline `{/* i18n-freeze *\/}...{/* /i18n-freeze *\/}` pattern. */
const INLINE_FREEZE_RE = /\{\/\*\s*i18n-freeze(?:-start)?\s*\*\/\}[\s\S]*?\{\/\*\s*(?:\/i18n-freeze|i18n-freeze-end|i18n-unfreeze)\s*\*\/\}/g;
const FREEZE_START_LINE_RE = /^\s*\{\/\*\s*i18n-freeze(?:-start)?\s*\*\/\}\s*$/;
const FREEZE_END_LINE_RE = /^\s*\{\/\*\s*(?:\/i18n-freeze|i18n-freeze-end|i18n-unfreeze)\s*\*\/\}\s*$/;

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
  fr: "French",
  zh: "Chinese (Simplified)",
  ja: "Japanese",
  ar: "Arabic",
  pt: "Portuguese (Brazil)",
});

const RTL_LOCALES = Object.freeze(["ar"]);

function isRtlLocale(locale) {
  return RTL_LOCALES.includes(locale);
}

function getLocaleDirection(locale) {
  return isRtlLocale(locale) ? "rtl" : "ltr";
}

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

/** Splits plain text further so glossary terms and Stellar addresses stay untranslated. */
function protectGlossary(text, glossary) {
  if (!text) return [{ protected: false, text }];
  const terms = [...glossary].sort((a, b) => b.length - a.length).map(escapeRegExp);
  const patterns = ["\\b[CG][A-Z2-7]{55}\\b"];
  if (terms.length > 0) {
    patterns.push(`(?<![\\p{L}\\p{N}_-])(?:${terms.join("|")})(?![\\p{L}\\p{N}_])`);
  }
  const re = new RegExp(patterns.join("|"), "gu");
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
 * inline `{/* i18n-freeze *\/}...{/* /i18n-freeze *\/}` spans, inline code,
 * JSX tags (minus translatable attribute values), `{expressions}`,
 * link targets, raw URLs, HTML entities, table separator rows, Stellar
 * contract/account addresses, and glossary terms.
 */
function tokenizeProse(text, glossary = DEFAULT_GLOSSARY) {
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

    // Inline {/* i18n-freeze */}...{/* /i18n-freeze */}
    if (ch === "{" && text.slice(i, i + 3) === "{/*") {
      INLINE_FREEZE_RE.lastIndex = i;
      const freezeMatch = INLINE_FREEZE_RE.exec(text);
      if (freezeMatch && freezeMatch.index === i) {
        protect(freezeMatch[0]);
        i += freezeMatch[0].length;
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
 * Splits an MDX document into blocks: `protected` (copied verbatim, including
 * `{/* i18n-freeze *\/}` blocks and fenced code blocks), `metadata`
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
  const push = (type, from, to, extra = {}) => {
    flushProse();
    blocks.push({ type, text: lines.slice(from, to + 1).join("\n"), ...extra });
  };

  for (let l = 0; l < lines.length; l++) {
    const line = lines[l];

    // Check for {/* i18n-freeze */} block directive
    if (FREEZE_START_LINE_RE.test(line)) {
      let closeIdx = -1;
      for (let scan = l + 1; scan < lines.length; scan++) {
        if (FREEZE_END_LINE_RE.test(lines[scan])) {
          closeIdx = scan;
          break;
        }
        if (FREEZE_START_LINE_RE.test(lines[scan])) {
          break;
        }
      }
      if (closeIdx !== -1) {
        push("protected", l, closeIdx, { frozen: true });
        l = closeIdx;
        continue;
      }
      // Single-tag {/* i18n-freeze */} freezes the immediately following block
      let nextIdx = l + 1;
      while (nextIdx < lines.length && lines[nextIdx].trim() === "") nextIdx++;
      if (nextIdx < lines.length) {
        const nextFence = /^(\s*)(`{3,}|~{3,})/.exec(lines[nextIdx]);
        if (nextFence) {
          const marker = nextFence[2];
          let end = nextIdx + 1;
          const closer = new RegExp(`^\\s*${marker[0] === "`" ? "`" : "~"}{${marker.length},}\\s*$`);
          while (end < lines.length && !closer.test(lines[end])) end++;
          const finalEnd = Math.min(end, lines.length - 1);
          push("protected", l, finalEnd, { frozen: true });
          l = finalEnd;
          continue;
        }
        let end = nextIdx;
        while (end + 1 < lines.length && lines[end + 1].trim() !== "") end++;
        push("protected", l, end, { frozen: true });
        l = end;
        continue;
      }
      push("protected", l, l, { frozen: true });
      continue;
    }

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

// ── Translation Memory (SHA-256 content hash) ───────────────────────────────

/** Computes a full 64-character SHA-256 hex content hash for a segment or document. */
function contentHash(text) {
  return crypto.createHash("sha256").update(String(text)).digest("hex");
}

function createTranslationMemory(initial = {}) {
  return {
    version: 1,
    entries: initial && typeof initial.entries === "object" && initial.entries !== null ? { ...initial.entries } : {},
  };
}

function loadTranslationMemory(filePath) {
  if (!filePath || !fs.existsSync(filePath)) return createTranslationMemory();
  try {
    const parsed = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return createTranslationMemory(parsed);
  } catch {
    return createTranslationMemory();
  }
}

function saveTranslationMemory(filePath, memory) {
  const dir = path.dirname(filePath);
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(memory, null, 2)}\n`, "utf8");
}

function getCachedTranslation(memory, template, locale) {
  if (!memory || !memory.entries) return null;
  const hash = contentHash(template);
  const entry = memory.entries[hash];
  if (!entry || !entry.translations) return null;
  return typeof entry.translations[locale] === "string" ? entry.translations[locale] : null;
}

function setCachedTranslation(memory, template, locale, translated) {
  if (!memory) return;
  if (!memory.entries) memory.entries = {};
  const hash = contentHash(template);
  const existing = memory.entries[hash] ?? {
    sourceHash: hash,
    source: template,
    translations: {},
  };
  existing.translations[locale] = translated;
  memory.entries[hash] = existing;
}

/**
 * Translates an MDX document. `translateBatch(templates, locale)` receives
 * placeholder templates and must resolve to translations in the same order.
 * Segments are sent in chunks of at most `maxBatchChars` characters.
 * When `translationMemory` is provided, segments are looked up and stored by
 * SHA-256 content hash so unchanged segments never hit the provider API.
 */
async function translateDocument(source, options) {
  const {
    locale,
    translateBatch,
    glossary = DEFAULT_GLOSSARY,
    maxBatchChars = 6000,
    retries = 1,
    translationMemory = null,
  } = options;
  const blocks = parseBlocks(source);
  const plan = blocks.map((block) => {
    if (block.type === "prose") return { block, segments: [toSegment(tokenizeProse(block.text, glossary))] };
    if (block.type === "metadata") return { block, segments: metadataSegments(block.text, glossary) };
    return { block, segments: [] };
  });

  const allTranslatable = plan.flatMap((p) => p.segments.filter((s) => s.translatable));
  const results = new Map();
  const pending = [];

  for (const segment of allTranslatable) {
    const cached = getCachedTranslation(translationMemory, segment.template, locale);
    if (cached !== null) {
      try {
        results.set(segment, restoreSegment(segment, cached));
        continue;
      } catch {
        // Cached translation failed placeholder check; re-translate
      }
    }
    pending.push(segment);
  }

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
        batch.forEach((segment, i) => {
          const rawTranslated = String(translations[i]);
          const restored = restoreSegment(segment, rawTranslated);
          results.set(segment, restored);
          if (translationMemory) {
            setCachedTranslation(translationMemory, segment.template, locale, rawTranslated);
          }
        });
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
  return parseBlocks(text).flatMap((b) => {
    if (b.type !== "protected") return [];
    if (/^\s*(`{3,}|~{3,})/.test(b.text)) return [b.text];
    if (b.frozen) {
      return [...b.text.matchAll(/^(\s*(`{3,}|~{3,})[^\n]*\n[\s\S]*?^\s*\2\s*$)/gm)].map((m) => m[1]);
    }
    return [];
  });
}

function extractFrozenBlocks(text) {
  const blockLevel = parseBlocks(text)
    .filter((b) => b.type === "protected" && b.frozen)
    .map((b) => b.text);
  const inlineLevel = [...stripFences(text).matchAll(INLINE_FREEZE_RE)].map((m) => m[0]);
  return [...blockLevel, ...inlineLevel];
}

function extractContractAddresses(text) {
  return [...text.matchAll(STELLAR_ADDRESS_RE)].map((m) => m[0]);
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
  compare("Frozen i18n blocks", extractFrozenBlocks(source), extractFrozenBlocks(target));
  compare("Contract addresses", extractContractAddresses(source), extractContractAddresses(target));
  if (/⟦\d+⟧/.test(target)) errors.push("Unrestored placeholder found in translation");
  return errors;
}

// ── Cross-file terminology consistency checker ──────────────────────────────

function extractHeadings(text) {
  return [...stripFences(text).matchAll(/^(#{1,6})\s+(.+)$/gm)].map((m) => ({
    level: m[1].length,
    text: m[2].trim(),
  }));
}

function extractBoldTerms(text) {
  return [...stripFences(text).matchAll(/\*\*([^*\n]+)\*\*/g)].map((m) => m[1].trim());
}

function extractMetadataTitle(text) {
  const metaBlock = parseBlocks(text).find((b) => b.type === "metadata");
  if (!metaBlock) return null;
  const match = /\btitle\s*:\s*(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)')/.exec(metaBlock.text);
  return match ? (match[1] ?? match[2] ?? null) : null;
}

/**
 * Checks terminology consistency across translated files of the same locale.
 * Flags when the same English heading, bolded domain term, metadata title,
 * tracked glossary term, or identical English sentence is translated
 * differently across files for the same locale.
 */
function checkTerminologyConsistency(documents, options = {}) {
  const trackedTerms = options.trackedTerms ?? [];
  // Map: locale -> englishTerm -> Map<translation, Set<route>>
  const byLocale = new Map();

  const recordMapping = (locale, englishTerm, translation, route) => {
    const cleanEn = String(englishTerm).trim();
    const cleanTr = String(translation).trim();
    if (!cleanEn || !cleanTr) return;
    if (!byLocale.has(locale)) byLocale.set(locale, new Map());
    const localeMap = byLocale.get(locale);
    if (!localeMap.has(cleanEn)) localeMap.set(cleanEn, new Map());
    const variantMap = localeMap.get(cleanEn);
    if (!variantMap.has(cleanTr)) variantMap.set(cleanTr, new Set());
    variantMap.get(cleanTr).add(route);
  };

  for (const doc of documents) {
    const locale = doc.locale;
    const route = doc.route ?? doc.file ?? "unknown";
    const source = stripProvenance(doc.source ?? "");
    const translated = stripProvenance(doc.translated ?? "");
    if (!locale || !source || !translated) continue;

    // 1. Metadata title alignment
    const srcTitle = extractMetadataTitle(source);
    const dstTitle = extractMetadataTitle(translated);
    if (srcTitle && dstTitle) {
      recordMapping(locale, srcTitle, dstTitle, route);
    }

    // 2. Heading alignment
    const srcHeadings = extractHeadings(source);
    const dstHeadings = extractHeadings(translated);
    if (srcHeadings.length === dstHeadings.length) {
      for (let i = 0; i < srcHeadings.length; i++) {
        if (srcHeadings[i].level === dstHeadings[i].level) {
          recordMapping(locale, srcHeadings[i].text, dstHeadings[i].text, route);
        }
      }
    }

    // 3. Bolded domain term alignment within matching prose blocks
    const srcProseBlocks = parseBlocks(source).filter((b) => b.type === "prose");
    const dstProseBlocks = parseBlocks(translated).filter((b) => b.type === "prose");
    if (srcProseBlocks.length === dstProseBlocks.length) {
      for (let i = 0; i < srcProseBlocks.length; i++) {
        const srcBolds = extractBoldTerms(srcProseBlocks[i].text);
        const dstBolds = extractBoldTerms(dstProseBlocks[i].text);
        if (srcBolds.length > 0 && srcBolds.length === dstBolds.length) {
          for (let j = 0; j < srcBolds.length; j++) {
            recordMapping(locale, srcBolds[j], dstBolds[j], route);
          }
        }
        // Identical single-line prose sentences
        const srcLine = srcProseBlocks[i].text.trim();
        const dstLine = dstProseBlocks[i].text.trim();
        if (srcLine && !srcLine.includes("\n") && !dstLine.includes("\n")) {
          recordMapping(locale, srcLine, dstLine, route);
        }
      }
    }

    // 4. Explicit tracked terms mapping (e.g., term: [candidate1, candidate2])
    if (trackedTerms && typeof trackedTerms === "object" && !Array.isArray(trackedTerms)) {
      for (const [enTerm, localeDict] of Object.entries(trackedTerms)) {
        const candidates = Array.isArray(localeDict) ? localeDict : (localeDict?.[locale] ?? []);
        const enRe = new RegExp(`\\b${escapeRegExp(enTerm)}\\b`, "i");
        if (enRe.test(source)) {
          for (const candidate of candidates) {
            const candRe = new RegExp(`(?<![\\p{L}\\p{N}_])${escapeRegExp(candidate)}(?![\\p{L}\\p{N}_])`, "iu");
            if (candRe.test(translated)) {
              recordMapping(locale, enTerm, candidate, route);
            }
          }
        }
      }
    }
  }

  const inconsistencies = [];
  for (const [locale, localeMap] of byLocale.entries()) {
    for (const [englishTerm, variantMap] of localeMap.entries()) {
      if (variantMap.size > 1) {
        const variants = [...variantMap.entries()].map(([translation, routes]) => ({
          translation,
          routes: [...routes].sort(),
        }));
        const summary = variants.map((v) => `"${v.translation}" (${v.routes.join(", ")})`).join(" vs ");
        inconsistencies.push({
          locale,
          englishTerm,
          variants,
          message: `Inconsistent [${locale}] translation for "${englishTerm}": ${summary}`,
        });
      }
    }
  }
  return inconsistencies;
}

// ── Glossary Auto-Validator ─────────────────────────────────────────────────

/**
 * Validates `translation-glossary.json` and scans English MDX sources for
 * unregistered technical terms/acronyms. Unknown technical terms trigger a CI
 * failure when `--check` or `--validate-glossary` runs.
 */
function validateGlossary(glossaryInput, docsSources = [], options = {}) {
  const errors = [];
  let rawTerms = [];
  let allowedTerms = [];

  if (Array.isArray(glossaryInput)) {
    rawTerms = glossaryInput;
  } else if (glossaryInput && typeof glossaryInput === "object") {
    if (!Array.isArray(glossaryInput.terms)) {
      errors.push("translation-glossary.json must contain a `terms` array.");
    } else {
      rawTerms = glossaryInput.terms;
    }
    if (Array.isArray(glossaryInput.allowedTerms)) {
      allowedTerms = glossaryInput.allowedTerms;
    }
  } else {
    errors.push("Glossary input must be an array or an object with a `terms` array.");
  }

  const seen = new Set();
  for (const term of rawTerms) {
    if (typeof term !== "string" || !term.trim()) {
      errors.push(`Invalid glossary term: ${JSON.stringify(term)}`);
      continue;
    }
    if (seen.has(term)) {
      errors.push(`Duplicate glossary term: "${term}"`);
    }
    seen.add(term);
  }

  const baseGlossary = options.baseGlossary ?? DEFAULT_GLOSSARY;
  const knownTerms = new Set([...baseGlossary, ...rawTerms, ...allowedTerms]);
  // Also allow pluralized acronyms (e.g. "RWAs" when "RWA" is in the glossary)
  for (const term of [...knownTerms]) {
    knownTerms.add(`${term}s`);
  }

  const unknownTerms = new Set();
  for (const item of docsSources) {
    const route = typeof item === "string" ? "document" : (item.route ?? item.file ?? "document");
    const source = typeof item === "string" ? item : (item.source ?? "");
    const proseBlocks = parseBlocks(source).filter((b) => b.type === "prose");

    for (const block of proseBlocks) {
      const clean = block.text
        .replace(INLINE_FREEZE_RE, " ")
        .replace(/(`+)[\s\S]*?\1/g, " ")
        .replace(/<[^>]+>/g, " ")
        .replace(/\{[^}]*\}/g, " ")
        .replace(/\]\([^)]*\)/g, "] ")
        .replace(/https?:\/\/\S+/g, " ")
        .replace(STELLAR_ADDRESS_RE, " ");

      // Check all-caps technical acronyms (3+ chars)
      for (const match of clean.matchAll(/\b[A-Z][A-Z0-9]{2,}\b/g)) {
        const candidate = match[0];
        if (!knownTerms.has(candidate)) {
          unknownTerms.add(candidate);
          errors.push(`Unknown technical term "${candidate}" in ${route}; add it to scripts/translation-glossary.json`);
        }
      }
    }

    // If translated text is also provided, verify glossary preservation
    if (typeof item === "object" && item && item.translated) {
      const coverage = computeTerminologyCoverage(source, item.translated, [...baseGlossary, ...rawTerms]);
      for (const missingTerm of coverage.missing) {
        errors.push(`Glossary term "${missingTerm}" missing in translated ${route} [${item.locale ?? "unknown"}]`);
      }
    }
  }

  return {
    valid: errors.length === 0,
    errors,
    unknownTerms: [...unknownTerms].sort(),
  };
}

// ── Quality Metrics (BLEU-4 & Terminology Coverage) ─────────────────────────

function tokenizeForBleu(text) {
  const clean = stripProvenance(String(text ?? ""))
    .replace(/```[\s\S]*?```/g, " ")
    .toLowerCase();
  return clean.match(/[\p{L}\p{N}]+|[^\s\p{L}\p{N}]/gu) ?? [];
}

function getNgramCounts(tokens, n) {
  const counts = new Map();
  for (let i = 0; i <= tokens.length - n; i++) {
    const key = tokens.slice(i, i + n).join("\u0000");
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return counts;
}

/**
 * Computes the BLEU-4 score (0..1) between a reference and candidate text
 * with brevity penalty and add-1 smoothing for n >= 2.
 */
function computeBleuScore(reference, candidate, maxN = 4) {
  const refTokens = tokenizeForBleu(reference);
  const candTokens = tokenizeForBleu(candidate);
  if (candTokens.length === 0 || refTokens.length === 0) return 0;

  const effectiveMaxN = Math.min(maxN, refTokens.length, candTokens.length);
  if (effectiveMaxN === 0) return 0;

  let logPrecisionSum = 0;
  for (let n = 1; n <= effectiveMaxN; n++) {
    const refCounts = getNgramCounts(refTokens, n);
    const candCounts = getNgramCounts(candTokens, n);
    let clipped = 0;
    let total = 0;
    for (const [ngram, count] of candCounts.entries()) {
      total += count;
      clipped += Math.min(count, refCounts.get(ngram) ?? 0);
    }
    const precision = n === 1 ? clipped / Math.max(1, total) : (clipped + 1) / (total + 1);
    if (precision <= 0) return 0;
    logPrecisionSum += Math.log(precision) / effectiveMaxN;
  }

  const brevityPenalty =
    candTokens.length >= refTokens.length
      ? 1
      : Math.exp(1 - refTokens.length / candTokens.length);

  return Number((brevityPenalty * Math.exp(logPrecisionSum)).toFixed(4));
}

/**
 * Computes terminology coverage (0..1): the fraction of glossary terms appearing
 * in the English source prose that are preserved in the translated prose.
 */
function computeTerminologyCoverage(source, translated, glossary = DEFAULT_GLOSSARY) {
  const srcProse = stripFences(stripProvenance(source)).replace(/(`+)[\s\S]*?\1/g, " ");
  const dstProse = stripFences(stripProvenance(translated)).replace(/(`+)[\s\S]*?\1/g, " ");

  const matched = [];
  const missing = [];
  const uniqueTerms = [...new Set(glossary)];

  for (const term of uniqueTerms) {
    const re = new RegExp(`(?<![\\p{L}\\p{N}_-])${escapeRegExp(term)}(?![\\p{L}\\p{N}_])`, "u");
    if (re.test(srcProse)) {
      if (re.test(dstProse)) {
        matched.push(term);
      } else {
        missing.push(term);
      }
    }
  }

  const total = matched.length + missing.length;
  return {
    coverage: total === 0 ? 1 : Number((matched.length / total).toFixed(4)),
    matched,
    missing,
    total,
  };
}

// ── Translated MDX Linter (broken links & code block validity) ──────────────

/**
 * Lint-checks translated MDX for unbalanced or invalid fenced code blocks,
 * broken markdown links, and structural parity issues.
 */
function lintTranslatedMdx(source, translated, options = {}) {
  const errors = [];
  const cleanTranslated = stripProvenance(translated);

  // 1. Check balanced fenced code blocks
  const lines = cleanTranslated.split("\n");
  let openFence = null;
  let openFenceLang = "";
  let openFenceLine = 0;
  let fenceContent = [];
  const codeBlocks = [];

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!openFence) {
      const m = /^(\s*)(`{3,}|~{3,})([^\n]*)$/.exec(line);
      if (m) {
        openFence = m[2];
        openFenceLang = m[3].trim().split(/\s+/)[0].toLowerCase();
        openFenceLine = i + 1;
        fenceContent = [];
      }
    } else {
      const closer = new RegExp(`^\\s*${openFence[0] === "`" ? "`" : "~"}{${openFence.length},}\\s*$`);
      if (closer.test(line)) {
        codeBlocks.push({ lang: openFenceLang, code: fenceContent.join("\n"), line: openFenceLine });
        openFence = null;
      } else {
        fenceContent.push(line);
      }
    }
  }

  if (openFence) {
    errors.push(`Unclosed fenced code block starting at line ${openFenceLine}`);
  }

  // 2. Validate JSON and TypeScript/JavaScript code block syntax
  let tsModule = null;
  try {
    tsModule = require("typescript");
  } catch {
    tsModule = null;
  }

  for (const block of codeBlocks) {
    if (block.lang === "json" && !block.code.includes("...")) {
      try {
        JSON.parse(block.code);
      } catch (err) {
        errors.push(`Invalid JSON in code block at line ${block.line}: ${err.message}`);
      }
    } else if (tsModule && ["ts", "tsx", "js", "jsx", "typescript", "javascript"].includes(block.lang)) {
      const result = tsModule.transpileModule(block.code, {
        reportDiagnostics: true,
        fileName: `sample.${block.lang.includes("x") ? "tsx" : "ts"}`,
        compilerOptions: {
          target: tsModule.ScriptTarget.ES2022,
          jsx: tsModule.JsxEmit.Preserve,
        },
      });
      const syntaxDiagnostics = (result.diagnostics ?? []).filter(
        (d) => d.category === tsModule.DiagnosticCategory.Error
      );
      if (syntaxDiagnostics.length > 0) {
        const msg = tsModule.flattenDiagnosticMessageText(syntaxDiagnostics[0].messageText, " ");
        errors.push(`Code block syntax error at line ${block.line}: ${msg}`);
      }
    }
  }

  // 3. Check markdown links for empty/malformed targets or missing internal routes
  const proseNoCode = stripFences(cleanTranslated).replace(/(`+)[\s\S]*?\1/g, "``");
  for (const match of proseNoCode.matchAll(/\[([^\]]*)\]\(([^)]*)\)/g)) {
    const rawTarget = match[2].trim();
    if (!rawTarget) {
      errors.push(`Empty link target for [${match[1]}]()`);
      continue;
    }
    const url = rawTarget.split(/\s+/)[0];
    if (url.startsWith("http://") || url.startsWith("https://")) {
      try {
        new URL(url);
      } catch {
        errors.push(`Malformed external link URL: "${url}"`);
      }
    } else if (url.startsWith("/docs/") && options.docsRoot) {
      const cleanRoute = url.split("#")[0].split("?")[0].replace(/\/+$/, "").replace(/^\/docs\/?/, "");
      const candidateMdx = path.join(options.docsRoot, "app", "[lang]", "docs", ...cleanRoute.split("/"), "page.mdx");
      const candidateEnMdx = path.join(options.docsRoot, "app", "[lang]", "docs", ...cleanRoute.split("/"), "page.en.mdx");
      const candidateTsx = path.join(options.docsRoot, "app", "[lang]", "docs", ...cleanRoute.split("/"), "page.tsx");
      if (!fs.existsSync(candidateMdx) && !fs.existsSync(candidateEnMdx) && !fs.existsSync(candidateTsx)) {
        errors.push(`Broken internal docs link: "${url}"`);
      }
    }
  }

  // Detect unclosed markdown links like `[text](https://example.com` without closing `)`
  for (const line of proseNoCode.split("\n")) {
    const stripped = line.replace(/\[[^\]]*\]\([^)]*\)/g, "");
    if (/\[[^\]]+\]\([^)\s]+$/.test(stripped)) {
      errors.push(`Unclosed markdown link in line: "${line.trim()}"`);
    }
  }

  if (source) {
    const parityErrors = validateParity(source, cleanTranslated);
    for (const err of parityErrors) {
      if (!errors.includes(err)) errors.push(err);
    }
  }

  return {
    valid: errors.length === 0,
    errors,
  };
}

/**
 * Generates a structured translation quality report and PR comment Markdown
 * table containing BLEU scores, terminology coverage, consistency checks, and
 * MDX lint status.
 */
function generateQualityReport(records, options = {}) {
  const glossary = options.glossary ?? DEFAULT_GLOSSARY;
  const consistencyIssues = options.consistencyIssues ?? checkTerminologyConsistency(records, options);
  const glossaryValidation = options.glossaryValidation ?? { valid: true, errors: [], unknownTerms: [] };

  const files = records.map((rec) => {
    const source = rec.source ?? "";
    const translated = stripProvenance(rec.translated ?? "");
    const reference = rec.reference ? stripProvenance(rec.reference) : source;
    const bleuScore = computeBleuScore(reference, translated);
    const termCoverage = computeTerminologyCoverage(source, translated, glossary);
    const parityErrors = rec.parityErrors ?? validateParity(source, translated);
    const lintResult = rec.lintErrors ? { valid: rec.lintErrors.length === 0, errors: rec.lintErrors } : lintTranslatedMdx(source, translated, options);

    return {
      route: rec.route ?? "unknown",
      locale: rec.locale ?? "en",
      direction: getLocaleDirection(rec.locale ?? "en"),
      bleuScore,
      terminologyCoverage: termCoverage.coverage,
      missingTerms: termCoverage.missing,
      parityErrors,
      lintErrors: lintResult.errors,
      passed: parityErrors.length === 0 && lintResult.errors.length === 0 && termCoverage.missing.length === 0,
    };
  });

  const avgBleu =
    files.length > 0
      ? Number((files.reduce((sum, f) => sum + f.bleuScore, 0) / files.length).toFixed(4))
      : 1;
  const avgCoverage =
    files.length > 0
      ? Number((files.reduce((sum, f) => sum + f.terminologyCoverage, 0) / files.length).toFixed(4))
      : 1;
  const passedCount = files.filter((f) => f.passed).length;
  const allPassed =
    passedCount === files.length &&
    consistencyIssues.length === 0 &&
    glossaryValidation.valid;

  const lines = [
    "## 🌐 MDX i18n Translation Quality Report",
    "",
    `- **Status:** ${allPassed ? "✅ Passed" : "❌ Issues Detected"}`,
    `- **Files Checked:** ${files.length} (${passedCount} passed)`,
    `- **Average BLEU Score:** ${(avgBleu * 100).toFixed(1)}%`,
    `- **Terminology Coverage:** ${(avgCoverage * 100).toFixed(1)}%`,
    `- **Cross-File Consistency:** ${consistencyIssues.length === 0 ? "100% consistent" : `${consistencyIssues.length} inconsistency(ies)`}`,
    `- **Glossary Validation:** ${glossaryValidation.valid ? "Valid" : `${glossaryValidation.errors.length} error(s)`}`,
    "",
  ];

  if (files.length > 0) {
    lines.push(
      "| Route | Locale | Dir | BLEU Score | Terminology Coverage | Status |",
      "| :--- | :---: | :---: | :---: | :---: | :---: |"
    );
    for (const f of files) {
      lines.push(
        `| \`${f.route}\` | \`${f.locale}\` | \`${f.direction}\` | ${(f.bleuScore * 100).toFixed(1)}% | ${(f.terminologyCoverage * 100).toFixed(1)}% | ${f.passed ? "✅ OK" : "❌ Failed"} |`
      );
    }
    lines.push("");
  }

  if (consistencyIssues.length > 0) {
    lines.push("### ⚠️ Cross-File Terminology Inconsistencies", "");
    for (const issue of consistencyIssues) {
      lines.push(`- ${issue.message}`);
    }
    lines.push("");
  }

  if (!glossaryValidation.valid) {
    lines.push("### ❌ Glossary Validation Errors", "");
    for (const err of glossaryValidation.errors) {
      lines.push(`- ${err}`);
    }
    lines.push("");
  }

  return {
    summary: {
      allPassed,
      totalFiles: files.length,
      passedFiles: passedCount,
      averageBleuScore: avgBleu,
      averageTerminologyCoverage: avgCoverage,
      consistencyIssueCount: consistencyIssues.length,
      glossaryErrorCount: glossaryValidation.errors.length,
    },
    files,
    consistencyIssues,
    glossaryValidation,
    markdown: lines.join("\n"),
  };
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
 *  - `gemini`    — Google Gemini `generateContent` REST API (`GEMINI_API_KEY`)
 *  - `anthropic` — Claude Messages API (`ANTHROPIC_API_KEY`)
 *  - `openai`    — any OpenAI-compatible Chat Completions endpoint (`OPENAI_API_KEY`, `OPENAI_BASE_URL`)
 *  - `pseudo`    — offline pseudo-translation
 */
function createProvider(options) {
  const { name, model, apiKey, baseUrl, glossary = DEFAULT_GLOSSARY, fetchImpl = globalThis.fetch } = options;
  if (name === "pseudo") {
    return async (templates, locale) => templates.map((t) => pseudoTranslate(t, locale));
  }
  if (name !== "anthropic" && name !== "openai" && name !== "gemini") {
    throw new Error(`Unknown provider "${name}" (expected gemini, anthropic, openai or pseudo)`);
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

  if (name === "gemini") {
    const effectiveModel = model || "gemini-2.0-flash";
    const rootUrl = (baseUrl ?? "https://generativelanguage.googleapis.com/v1beta").replace(/\/+$/, "");
    return async (templates, locale) => {
      const url = `${rootUrl}/models/${encodeURIComponent(effectiveModel)}:generateContent?key=${encodeURIComponent(apiKey)}`;
      const data = await call(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          systemInstruction: {
            parts: [{ text: systemPrompt(locale, glossary) }],
          },
          contents: [
            {
              role: "user",
              parts: [{ text: JSON.stringify(templates) }],
            },
          ],
          generationConfig: {
            temperature: 0,
            responseMimeType: "application/json",
          },
        }),
      });
      const text = (data.candidates?.[0]?.content?.parts ?? [])
        .map((part) => part.text ?? "")
        .join("");
      return parseJsonArray(text);
    };
  }

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
  RTL_LOCALES,
  isRtlLocale,
  getLocaleDirection,
  GENERATED_PAGE_MARKER,
  parseBlocks,
  tokenizeProse,
  toSegment,
  restoreSegment,
  translateDocument,
  validateParity,
  extractJsxTags,
  extractFrozenBlocks,
  extractContractAddresses,
  contentHash,
  createTranslationMemory,
  loadTranslationMemory,
  saveTranslationMemory,
  getCachedTranslation,
  setCachedTranslation,
  checkTerminologyConsistency,
  validateGlossary,
  computeBleuScore,
  computeTerminologyCoverage,
  lintTranslatedMdx,
  generateQualityReport,
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
