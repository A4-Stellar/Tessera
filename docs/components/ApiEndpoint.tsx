import type { ReactNode } from "react";

type Method = "GET" | "POST" | "PUT" | "DELETE";

const METHOD_STYLES: Record<Method, string> = {
  GET: "bg-brand-500/15 text-brand-300 border-brand-500/30",
  POST: "bg-sky-500/15 text-sky-300 border-sky-500/30",
  PUT: "bg-gold-500/15 text-gold-300 border-gold-500/30",
  DELETE: "bg-red-500/15 text-red-300 border-red-500/30",
};

interface ApiEndpointProps {
  method?: Method;
  path: string;
  description?: ReactNode;
}

/** A method + path banner used to head an API reference section. */
export function ApiEndpoint({ method = "GET", path, description }: ApiEndpointProps) {
  return (
    <div className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4">
      <div className="flex flex-wrap items-center gap-3">
        <span className={`rounded-md border px-2.5 py-1 font-mono text-xs font-bold ${METHOD_STYLES[method]}`}>
          {method}
        </span>
        <code className="font-mono text-sm text-base-100">{path}</code>
      </div>
      {description && <p className="mt-2 text-sm text-base-200/75">{description}</p>}
    </div>
  );
}

export default ApiEndpoint;
