/**
 * Client-side handling of API version deprecation headers (issue: API versioning).
 *
 * The server annotates deprecated-but-served responses with:
 *   Sunset: 2027-06-30
 *   Link: </v1/meta/versions>; rel="successor-version"
 *
 * These helpers surface that signal once per (path, sunset) pair so chatty
 * clients do not flood the console, and stop the SDK from silently relying on
 * endpoints that are scheduled for removal.
 */

/** Already-warned (request path, sunset) pairs. */
const warned = new Set<string>();

/**
 * Extract the URI from a `Link` header whose `rel` is `successor-version`.
 * Pure and dependency-free so it can be unit-tested directly. Handles
 * multiple comma-separated links, single or double quotes, and casing.
 */
export function parseSuccessor(link: string): string | null {
  const match = link.match(
    /<([^>]+)>\s*;\s*rel\s*=\s*["']successor-version["']/i,
  );
  return match ? match[1] : null;
}

/** Best-effort pathname for a response URL (falls back to the raw URL). */
function responsePath(url: string): string {
  if (!url) return "<unknown>";
  try {
    return new URL(url).pathname;
  } catch {
    return url;
  }
}

/**
 * Emit a `console.warn` describing a deprecated response, at most once per
 * (path, sunset). Responses without deprecation headers are ignored.
 */
export function warnIfDeprecated(response: Response): void {
  const sunset = response.headers.get("sunset");
  const link = response.headers.get("link") ?? "";
  const successor = parseSuccessor(link);

  if (!sunset && !successor) return;

  const path = responsePath(response.url);
  const key = `${path}\u0000${sunset ?? ""}`;
  if (warned.has(key)) return;
  warned.add(key);

  const details: string[] = [`Tessera API endpoint ${path} is deprecated`];
  if (sunset) details.push(`sunset: ${sunset}`);
  if (successor) details.push(`successor: ${successor}`);
  console.warn(`${details.join(" — ")}. Migrate before the sunset date.`);
}

/** `fetch` wrapper that warns when the response advertises deprecation. */
export async function fetchWithDeprecationWarning(
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> {
  const response = await fetch(input, init);
  warnIfDeprecated(response);
  return response;
}
