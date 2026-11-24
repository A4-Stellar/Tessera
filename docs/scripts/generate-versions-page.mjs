#!/usr/bin/env node
/**
 * Generate docs/content/en/docs/api-versions.mdx from api/deprecation_map.toml.
 *
 * Plain Node ESM, no dependencies. Understands the same restricted TOML
 * subset as api/src/middleware/deprecation.rs ([versions], [[deprecated]],
 * quoted strings, blank lines, # comments).
 *
 * Usage:
 *   node docs/scripts/generate-versions-page.mjs            # write the page
 *   node docs/scripts/generate-versions-page.mjs --check    # verify it is current
 *   node docs/scripts/generate-versions-page.mjs --out FILE # write elsewhere
 */

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
// Script lives in docs/scripts/; the map lives in <repo>/api/.
const MAP_PATH = path.resolve(__dirname, '../../api/deprecation_map.toml');
const DEFAULT_OUT = path.resolve(
  __dirname,
  '../content/en/docs/api-versions.mdx',
);

function fail(line, message) {
  throw new Error(`deprecation_map.toml:${line}: ${message}`);
}

/** Remove a # comment, ignoring # characters inside quoted strings. */
function stripComment(line) {
  let quote = null;
  let escaped = false;
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (quote === '"' && ch === '\\') {
      escaped = true;
      continue;
    }
    if ((quote === '"' || quote === "'") && ch === quote) {
      quote = null;
      continue;
    }
    if (quote === null && (ch === '"' || ch === "'")) {
      quote = ch;
      continue;
    }
    if (quote === null && ch === '#') return line.slice(0, i);
  }
  return line;
}

function unquote(raw, line) {
  const value = raw.trim();
  const quote = value[0];
  if (quote !== '"' && quote !== "'") {
    fail(line, `expected a quoted string, found \`${value}\``);
  }
  if (value.length < 2 || !value.endsWith(quote)) {
    fail(line, `unterminated string: \`${value}\``);
  }
  const inner = value.slice(1, -1);
  if (quote === "'") return inner;
  return inner.replace(/\\(["\\nrt])/g, (_match, ch) => {
    if (ch === 'n') return '\n';
    if (ch === 'r') return '\r';
    if (ch === 't') return '\t';
    return ch;
  });
}

function validDate(value) {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const parsed = new Date(`${value}T00:00:00Z`);
  return !Number.isNaN(parsed.getTime()) && parsed.toISOString().slice(0, 10) === value;
}

function parseMap(text) {
  const versions = new Map();
  const deprecated = [];
  let section = null;
  let pending = null;
  let pendingLine = 0;

  const finishPending = () => {
    if (pending === null) return;
    for (const key of ['path', 'sunset', 'successor', 'docs_url']) {
      if (pending[key] === undefined) {
        fail(pendingLine, `[[deprecated]] entry is missing \`${key}\``);
      }
    }
    if (!validDate(pending.sunset)) {
      fail(pendingLine, `sunset must be an ISO-8601 date, found \`${pending.sunset}\``);
    }
    deprecated.push(pending);
    pending = null;
  };

  text.split(/\r?\n/).forEach((raw, index) => {
    const line = index + 1;
    const text = stripComment(raw).trim();
    if (text === '') return;

    if (text === '[versions]') {
      finishPending();
      section = 'versions';
      return;
    }
    if (text === '[[deprecated]]') {
      finishPending();
      pending = {};
      pendingLine = line;
      section = 'deprecated';
      return;
    }
    if (text.startsWith('[')) {
      fail(line, `unsupported table header \`${text}\``);
    }

    const eq = text.indexOf('=');
    if (eq < 0) fail(line, `expected \`key = "value"\`, found \`${text}\``);
    const key = text.slice(0, eq).trim();
    const value = unquote(text.slice(eq + 1), line);

    if (section === 'versions') {
      if (key === '') fail(line, 'empty version name');
      versions.set(key, value);
    } else if (section === 'deprecated') {
      if (!['path', 'sunset', 'successor', 'docs_url'].includes(key)) {
        fail(line, `unknown deprecation key \`${key}\``);
      }
      if (pending[key] !== undefined) {
        fail(line, `duplicate deprecation key \`${key}\``);
      }
      pending[key] = value;
    } else {
      fail(line, 'key outside of a `[versions]` or `[[deprecated]]` table');
    }
  });
  finishPending();
  return { versions, deprecated };
}

/** Markdown table cell: keep pipes and newlines from breaking the table. */
function cell(value) {
  return String(value).replace(/\|/g, '\\|').replace(/\r?\n/g, ' ');
}

function earliestSunset(map, version) {
  const prefix = `/${version}/`;
  const dates = map.deprecated
    .filter((entry) => entry.path.startsWith(prefix))
    .map((entry) => entry.sunset)
    .sort();
  return dates.length > 0 ? dates[0] : null;
}

function render(map) {
  const versionRows = [...map.versions.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([name, status]) => {
      const sunset = earliestSunset(map, name);
      return `| \`${cell(name)}\` | \`${cell(status)}\` | ${sunset ? `\`${sunset}\`` : '—'} |`;
    })
    .join('\n');

  const deprecatedRows = map.deprecated
    .map(
      (entry) =>
        `| \`${cell(entry.path)}\` | \`${entry.sunset}\` | \`${cell(entry.successor)}\` | [Migration guide](${entry.docs_url}) |`,
    )
    .join('\n');

  return `export const metadata = {
  title: "API Versions",
  description: "Supported Tessera API versions, deprecation schedule, and migration paths.",
  canonical: "/docs/api-versions",
};

# API Versions

> **Auto-generated file — do not edit by hand.** Regenerate it with
> \`node docs/scripts/generate-versions-page.mjs\` after changing
> \`api/deprecation_map.toml\`.

The Tessera REST API versions data endpoints with a path prefix (\`/v1\` today,
\`/v2\` reserved for the next breaking release). Protocol and operations
endpoints such as \`/health\`, \`/metrics\`, \`/.well-known/stellar.toml\`, and
\`/graphql\` are not versioned.

## Supported versions

| Version | Status | Earliest sunset |
| :------ | :----- | :-------------- |
${versionRows}

## Deprecated paths

A request to a deprecated path receives a \`Sunset\` header and
\`Link: <successor>; rel="successor-version"\` until its sunset date. On and
after that date the API returns \`410 Gone\` with a \`migration_guide\` field
pointing at the documentation below. Paths ending in \`/*\` match every route
under that prefix.

| Deprecated path | Sunset | Successor | Migration guide |
| :-------------- | :----- | :-------- | :-------------- |
${deprecatedRows}

Clients can fetch this schedule at runtime from \`GET /v1/meta/versions\`.
`;
}

function parseArgs(argv) {
  let out = DEFAULT_OUT;
  let check = false;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--check') {
      check = true;
    } else if (arg === '--out') {
      out = argv[i + 1];
      if (!out) fail(0, '--out requires a path');
      i += 1;
    } else if (arg.startsWith('--out=')) {
      out = arg.slice('--out='.length);
    } else {
      fail(0, `unknown argument \`${arg}\``);
    }
  }
  return { out: path.resolve(out), check };
}

function main() {
  const { out, check } = parseArgs(process.argv.slice(2));
  const text = fs.readFileSync(MAP_PATH, 'utf8');
  const generated = render(parseMap(text));

  if (check) {
    let current = null;
    try {
      current = fs.readFileSync(out, 'utf8');
    } catch {
      current = null;
    }
    if (current !== generated) {
      console.error(`${path.relative(process.cwd(), out)} is stale — regenerate with: node docs/scripts/generate-versions-page.mjs`);
      process.exitCode = 1;
    } else {
      console.log(`${path.relative(process.cwd(), out)} is up to date.`);
    }
    return;
  }

  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.writeFileSync(out, generated);
  console.log(`Wrote ${path.relative(process.cwd(), out)}`);
}

main();
