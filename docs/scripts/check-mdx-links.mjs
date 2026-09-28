/**
 * MDX link validator (issue #132).
 *
 * Parses all markdown links (`[text](url)`) across every `.mdx`/`.md` file,
 * verifies internal relative links and `/docs/...` routes (including `#anchor`
 * headers) against the App Router pages, and performs parallelized HTTP HEAD
 * checks for external URLs.
 *
 * Fails with exit code 1 when broken links are detected, so it can gate
 * `npm run build` and CI.
 *
 * Usage:
 *   node scripts/check-mdx-links.mjs                  # full check (internal + external)
 *   node scripts/check-mdx-links.mjs --skip-external  # offline-safe (used by `npm run build`)
 *   node scripts/check-mdx-links.mjs --root ./docs --timeout 10000 --concurrency 10
 */

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";

const args = process.argv.slice(2);
const SKIP_EXTERNAL = args.includes("--skip-external");
const rootArg = args.find((a) => a.startsWith("--root="))?.split("=")[1];
const timeoutArg = Number(args.find((a) => a.startsWith("--timeout="))?.split("=")[1]);
const concurrencyArg = Number(args.find((a) => a.startsWith("--concurrency="))?.split("=")[1]);

const DOCS_ROOT = resolve(rootArg ?? join(dirname(new URL(import.meta.url).pathname), ".."));
const TIMEOUT_MS = Number.isFinite(timeoutArg) && timeoutArg > 0 ? timeoutArg : 10000;
const CONCURRENCY = Number.isFinite(concurrencyArg) && concurrencyArg > 0 ? concurrencyArg : 10;
const LOCALES = ["en", "es", "fr", "ja", "pt"];

const SKIP_DIRS = new Set(["node_modules", ".next", ".git", "public"]);

/** Strip fenced code blocks and inline code so sample URLs aren't checked. */
function stripCode(content) {
  return content
    .replace(/```[\s\S]*?```/g, "")
    .replace(/`[^`\n]*`/g, "``");
}

function getDocFiles(dir, results = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP_DIRS.has(entry)) continue;
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      getDocFiles(full, results);
    } else if (entry.endsWith(".mdx") || entry.endsWith(".md")) {
      results.push(full);
    }
  }
  return results;
}

function extractLinks(content) {
  const links = [];
  const linkRe = /\[[^\]]*\]\(([^)\s]+)(?:\s+["'][^"']*["'])?\)/g;
  let match;
  while ((match = linkRe.exec(content)) !== null) {
    links.push(match[1]);
  }
  const autolinkRe = /<((?:https?:\/\/)[^<>\s]+)>/g;
  while ((match = autolinkRe.exec(content)) !== null) {
    links.push(match[1]);
  }
  return links;
}

function slugify(header) {
  return header
    .trim()
    .toLowerCase()
    .replace(/`([^`]*)`/g, "$1")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[^a-z0-9 \-_]/g, "")
    .trim()
    .replace(/\s+/g, "-");
}

const headerCache = new Map();
function getAnchors(file) {
  if (headerCache.has(file)) return headerCache.get(file);
  const anchors = new Set();
  try {
    const content = readFileSync(file, "utf-8");
    for (const match of content.matchAll(/^#{1,6}\s+(.+)$/gm)) {
      anchors.add(slugify(match[1]));
    }
  } catch {
    // Unreadable file: no anchors; the missing-file check reports it.
  }
  headerCache.set(file, anchors);
  return anchors;
}

/** Resolve an internal `/docs/...` route to its App Router page file. */
function resolveDocsRoute(routePath) {
  let clean = routePath.split("?")[0].replace(/\/+$/, "") || "/";
  if (clean !== "/") {
    const [maybeLocale, ...rest] = clean.slice(1).split("/");
    if (LOCALES.includes(maybeLocale)) clean = `/${rest.join("/")}`;
  }
  if (clean === "/") return { file: join(DOCS_ROOT, "app", "page.tsx"), exists: true };
  const sub = clean.replace(/^\/docs\/?/, "");
  if (clean === sub) return { file: null, exists: false }; // not a /docs route
  const candidates = [
    join(DOCS_ROOT, "app", "[lang]", "docs", ...sub.split("/"), "page.mdx"),
    join(DOCS_ROOT, "app", "[lang]", "docs", ...sub.split("/"), "page.tsx"),
    join(DOCS_ROOT, "app", "[lang]", "docs", `${sub || "index"}.mdx`),
  ];
  for (const file of candidates) {
    if (existsSync(file)) return { file, exists: true };
  }
  return { file: candidates[0], exists: false };
}

/** Resolve a relative link against the source file's directory. */
function resolveRelative(sourceFile, linkPath) {
  const base = resolve(dirname(sourceFile), linkPath.split("?")[0]);
  const candidates = [base, `${base}.mdx`, `${base}.md`, join(base, "page.mdx")];
  for (const file of candidates) {
    if (existsSync(file) && statSync(file).isFile()) return { file, exists: true };
  }
  return { file: base, exists: false };
}

async function checkExternal(url, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    let res = await fetch(url, { method: "HEAD", redirect: "follow", signal: controller.signal });
    if (res.status === 405 || res.status === 403 || res.status === 501) {
      res = await fetch(url, { method: "GET", redirect: "follow", signal: controller.signal });
    }
    // Consume/cancel the body so sockets are released on the GET fallback.
    if (res.body) await res.body.cancel().catch(() => {});
    return { ok: res.ok, status: res.status };
  } catch (err) {
    return { ok: false, status: 0, error: err.name === "AbortError" ? "timeout" : String(err.cause ?? err.message) };
  } finally {
    clearTimeout(timer);
  }
}

/** Bounded parallel map over items. */
async function pooledMap(items, concurrency, fn) {
  const results = new Array(items.length);
  let next = 0;
  async function worker() {
    while (next < items.length) {
      const i = next++;
      results[i] = await fn(items[i], i);
    }
  }
  await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, worker));
  return results;
}

const files = getDocFiles(DOCS_ROOT);
const internalFailures = [];
const externalJobs = []; // { url, file }

for (const file of files) {
  const rel = file.slice(DOCS_ROOT.length + 1).split(sep).join("/");
  const links = extractLinks(stripCode(readFileSync(file, "utf-8")));
  for (const raw of links) {
    const url = raw.trim();
    if (!url || url.startsWith("mailto:") || url.startsWith("tel:") || url.startsWith("data:")) continue;

    const [linkPath, anchor] = url.split("#", 2);

    if (url.startsWith("http://") || url.startsWith("https://")) {
      externalJobs.push({ url: linkPath, file: rel });
      continue;
    }
    if (url.startsWith("#")) {
      const anchors = getAnchors(file);
      if (!anchors.has(slugify(anchor ?? ""))) {
        internalFailures.push(`${rel}: anchor '#${anchor}' not found in own file`);
      }
      continue;
    }
    if (linkPath.startsWith("/")) {
      const { file: target, exists } = resolveDocsRoute(linkPath);
      if (!exists) {
        internalFailures.push(`${rel}: internal route '${linkPath}' has no page (looked for '${target}')`);
        continue;
      }
      if (anchor && target.endsWith(".mdx") && !getAnchors(target).has(slugify(anchor))) {
        internalFailures.push(`${rel}: anchor '#${anchor}' not found in '${linkPath}'`);
      }
      continue;
    }
    // Relative link.
    if (!linkPath) continue;
    const { file: target, exists } = resolveRelative(file, linkPath);
    if (!exists) {
      internalFailures.push(`${rel}: relative link '${linkPath}' does not exist`);
      continue;
    }
    if (anchor && /\.(mdx?|md)$/.test(target) && !getAnchors(target).has(slugify(anchor))) {
      internalFailures.push(`${rel}: anchor '#${anchor}' not found in '${linkPath}'`);
    }
  }
}

// Deduplicate external checks; report every source file on failure.
const byUrl = new Map();
for (const job of externalJobs) {
  if (!byUrl.has(job.url)) byUrl.set(job.url, []);
  byUrl.get(job.url).push(job.file);
}

let externalFailures = [];
if (!SKIP_EXTERNAL && byUrl.size > 0) {
  const entries = [...byUrl.entries()];
  const checked = await pooledMap(entries, CONCURRENCY, async ([url]) => ({
    url,
    ...(await checkExternal(url, TIMEOUT_MS)),
  }));
  for (const { url, ok, status, error } of checked) {
    if (!ok) {
      const detail = status ? `HTTP ${status}` : error;
      for (const file of byUrl.get(url)) {
        externalFailures.push(`${file}: external URL '${url}' unreachable (${detail})`);
      }
    }
  }
}

const failures = [...internalFailures, ...externalFailures];
if (failures.length > 0) {
  console.error(`Broken links detected (${failures.length}):`);
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  `All doc links valid (${files.length} files, ${externalJobs.length} external checked` +
    `${SKIP_EXTERNAL ? ", external skipped" : ""}).`,
);
