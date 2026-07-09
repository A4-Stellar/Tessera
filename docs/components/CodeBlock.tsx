"use client";

import { useState, type ReactNode } from "react";

interface CodeBlockProps {
  children: ReactNode;
  /** Optional filename / language label shown in the header bar. */
  title?: string;
  /** Raw text to copy; falls back to the rendered children when omitted. */
  code?: string;
}

/**
 * A titled code container with a copy button. For fenced code inside MDX, the
 * plain `pre`/`code` mapping is used; this component is for callouts where a
 * header and copy affordance add value.
 */
export function CodeBlock({ children, title, code }: CodeBlockProps) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    const text = code ?? (typeof children === "string" ? children : "");
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      /* clipboard unavailable */
    }
  }

  return (
    <div className="my-6 overflow-hidden rounded-xl border border-white/10 bg-[#0a0c11]">
      <div className="flex items-center justify-between border-b border-white/5 px-4 py-2">
        <span className="font-mono text-xs text-base-300">{title ?? "code"}</span>
        <button
          onClick={copy}
          className="text-xs font-medium text-base-300 transition-colors hover:text-brand-400"
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <pre className="overflow-x-auto p-4 text-sm leading-relaxed">
        <code className="font-mono text-base-100">{children}</code>
      </pre>
    </div>
  );
}

export default CodeBlock;
