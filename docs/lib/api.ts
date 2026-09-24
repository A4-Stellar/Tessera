/**
 * API base URL helpers shared by interactive, client-side documentation
 * components (e.g. `ApiPlayground`, `CapTableVisualizer`).
 *
 * Mirrors the `NEXT_PUBLIC_API_BASE_URL` convention documented in
 * `/docs/api/overview` and `.env.example`.
 */

/** Configured API base URL, defaulting to the local dev server. */
export const API_BASE_URL =
  process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";

export interface ApiEnvironmentPreset {
  label: string;
  /** Empty string means "not configured" — the field is left for the reader to fill in. */
  baseUrl: string;
}

/**
 * Environment presets for the interactive API playground. "Deployed Testnet"
 * uses `NEXT_PUBLIC_API_BASE_URL` when it points somewhere other than
 * localhost; otherwise it is left blank rather than inventing a URL.
 */
export const API_ENVIRONMENT_PRESETS: ApiEnvironmentPreset[] = [
  { label: "Localhost", baseUrl: "http://localhost:8080" },
  {
    label: "Deployed Testnet",
    baseUrl: API_BASE_URL.includes("localhost") ? "" : API_BASE_URL,
  },
];
