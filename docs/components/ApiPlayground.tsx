"use client";

import { useId, useState } from "react";
import { API_ENVIRONMENT_PRESETS } from "@/lib/api";
import { CodeBlock } from "@/components/CodeBlock";

type Method = "GET" | "POST" | "PUT" | "DELETE";

const METHODS: Method[] = ["GET", "POST", "PUT", "DELETE"];

interface QueryParam {
  key: string;
  value: string;
}

interface ApiPlaygroundProps {
  /** Pre-filled request path, e.g. `/assets/1/holders`. */
  defaultPath?: string;
  /** Pre-filled method. Defaults to `GET`. */
  defaultMethod?: Method;
}

interface PlaygroundResult {
  status: number;
  statusText: string;
  body: string;
}

function buildUrl(baseUrl: string, path: string, params: QueryParam[]): string {
  const trimmedBase = baseUrl.replace(/\/+$/, "");
  const trimmedPath = path.startsWith("/") ? path : `/${path}`;
  const search = params
    .filter((p) => p.key.trim().length > 0)
    .map((p) => `${encodeURIComponent(p.key)}=${encodeURIComponent(p.value)}`)
    .join("&");
  return `${trimmedBase}${trimmedPath}${search ? `?${search}` : ""}`;
}

/**
 * Client-side playground for trying Tessera API requests against a chosen
 * environment (localhost or the deployed testnet). Renders the live response
 * as formatted JSON.
 */
export function ApiPlayground({ defaultPath = "/stats", defaultMethod = "GET" }: ApiPlaygroundProps) {
  const selectId = useId();
  const [presetIndex, setPresetIndex] = useState(0);
  const [customBaseUrl, setCustomBaseUrl] = useState(API_ENVIRONMENT_PRESETS[0].baseUrl);
  const [method, setMethod] = useState<Method>(defaultMethod);
  const [path, setPath] = useState(defaultPath);
  const [params, setParams] = useState<QueryParam[]>([{ key: "", value: "" }]);
  const [body, setBody] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<PlaygroundResult | null>(null);

  function selectPreset(index: number) {
    setPresetIndex(index);
    setCustomBaseUrl(API_ENVIRONMENT_PRESETS[index].baseUrl);
  }

  function updateParam(index: number, field: keyof QueryParam, value: string) {
    setParams((prev) => prev.map((p, i) => (i === index ? { ...p, [field]: value } : p)));
  }

  function addParam() {
    setParams((prev) => [...prev, { key: "", value: "" }]);
  }

  function removeParam(index: number) {
    setParams((prev) => prev.filter((_, i) => i !== index));
  }

  async function execute() {
    if (!customBaseUrl) {
      setError("Set a base URL before sending a request.");
      setResult(null);
      return;
    }

    setLoading(true);
    setError(null);
    setResult(null);

    const url = buildUrl(customBaseUrl, path, params);
    const hasBody = method === "POST" || method === "PUT";

    try {
      const response = await fetch(url, {
        method,
        headers: hasBody && body ? { "Content-Type": "application/json" } : undefined,
        body: hasBody && body ? body : undefined,
      });
      const text = await response.text();
      let formatted = text;
      try {
        formatted = JSON.stringify(JSON.parse(text), null, 2);
      } catch {
        /* not JSON — show raw text */
      }
      setResult({ status: response.status, statusText: response.statusText, body: formatted });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Request failed. Check the URL and CORS settings.");
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4">
      <div className="flex flex-wrap items-end gap-3">
        <div>
          <label htmlFor={`${selectId}-env`} className="mb-1 block text-xs font-semibold text-base-300">
            Environment
          </label>
          <select
            id={`${selectId}-env`}
            value={presetIndex}
            onChange={(e) => selectPreset(Number(e.target.value))}
            className="rounded-md border border-white/10 bg-base-900 px-2 py-1.5 text-sm text-base-100"
          >
            {API_ENVIRONMENT_PRESETS.map((preset, index) => (
              <option key={preset.label} value={index}>
                {preset.label}
              </option>
            ))}
          </select>
        </div>

        <div className="min-w-[14rem] flex-1">
          <label htmlFor={`${selectId}-base`} className="mb-1 block text-xs font-semibold text-base-300">
            Base URL
          </label>
          <input
            id={`${selectId}-base`}
            type="text"
            value={customBaseUrl}
            onChange={(e) => setCustomBaseUrl(e.target.value)}
            placeholder="https://…"
            className="w-full rounded-md border border-white/10 bg-base-900 px-2 py-1.5 font-mono text-sm text-base-100"
          />
        </div>
      </div>

      <div className="mt-3 flex flex-wrap items-end gap-3">
        <div>
          <label htmlFor={`${selectId}-method`} className="mb-1 block text-xs font-semibold text-base-300">
            Method
          </label>
          <select
            id={`${selectId}-method`}
            value={method}
            onChange={(e) => setMethod(e.target.value as Method)}
            className="rounded-md border border-white/10 bg-base-900 px-2 py-1.5 font-mono text-sm text-base-100"
          >
            {METHODS.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
        </div>

        <div className="min-w-[14rem] flex-1">
          <label htmlFor={`${selectId}-path`} className="mb-1 block text-xs font-semibold text-base-300">
            Path
          </label>
          <input
            id={`${selectId}-path`}
            type="text"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="/assets/1/holders"
            className="w-full rounded-md border border-white/10 bg-base-900 px-2 py-1.5 font-mono text-sm text-base-100"
          />
        </div>
      </div>

      <fieldset className="mt-3">
        <legend className="mb-1 text-xs font-semibold text-base-300">Query parameters</legend>
        {params.map((param, index) => (
          <div key={index} className="mb-2 flex gap-2">
            <input
              type="text"
              aria-label={`Query parameter ${index + 1} name`}
              value={param.key}
              onChange={(e) => updateParam(index, "key", e.target.value)}
              placeholder="key"
              className="w-1/3 rounded-md border border-white/10 bg-base-900 px-2 py-1.5 font-mono text-sm text-base-100"
            />
            <input
              type="text"
              aria-label={`Query parameter ${index + 1} value`}
              value={param.value}
              onChange={(e) => updateParam(index, "value", e.target.value)}
              placeholder="value"
              className="flex-1 rounded-md border border-white/10 bg-base-900 px-2 py-1.5 font-mono text-sm text-base-100"
            />
            <button
              type="button"
              onClick={() => removeParam(index)}
              className="rounded-md border border-white/10 px-2 text-xs text-base-300 hover:text-red-300"
              aria-label={`Remove query parameter ${index + 1}`}
            >
              ✕
            </button>
          </div>
        ))}
        <button
          type="button"
          onClick={addParam}
          className="text-xs font-medium text-brand-400 hover:text-brand-300"
        >
          + Add parameter
        </button>
      </fieldset>

      {(method === "POST" || method === "PUT") && (
        <div className="mt-3">
          <label htmlFor={`${selectId}-body`} className="mb-1 block text-xs font-semibold text-base-300">
            JSON body
          </label>
          <textarea
            id={`${selectId}-body`}
            value={body}
            onChange={(e) => setBody(e.target.value)}
            rows={4}
            placeholder="{}"
            className="w-full rounded-md border border-white/10 bg-base-900 px-2 py-1.5 font-mono text-sm text-base-100"
          />
        </div>
      )}

      <button
        type="button"
        onClick={execute}
        disabled={loading}
        className="mt-4 rounded-md bg-brand-500 px-4 py-2 text-sm font-semibold text-base-950 transition-colors hover:bg-brand-400 disabled:cursor-not-allowed disabled:opacity-60"
      >
        {loading ? "Sending…" : "Send request"}
      </button>

      {error && (
        <p role="alert" className="mt-3 text-sm text-red-300">
          {error}
        </p>
      )}

      {result && (
        <div className="mt-4">
          <p className="mb-1 text-xs font-semibold text-base-300">
            {result.status} {result.statusText}
          </p>
          <CodeBlock title="response.json" code={result.body}>
            {result.body}
          </CodeBlock>
        </div>
      )}
    </div>
  );
}

export default ApiPlayground;
