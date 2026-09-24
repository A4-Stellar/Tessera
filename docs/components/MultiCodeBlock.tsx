"use client";

import { useState, useEffect, useId, useRef, useCallback, useMemo } from "react";

export interface CodeSnippet {
  /** Language identifier: e.g. "curl", "typescript", "javascript", "rust", "python", "json" */
  language: string;
  /** Human-readable tab title/label: e.g. "cURL", "TypeScript", "Rust", "Python" */
  label?: string;
  /** Code snippet text to display and copy */
  code: string;
  /** Optional file title or extra label */
  title?: string;
}

export interface MultiCodeBlockProps {
  /** Array of code snippets for each language */
  snippets?: CodeSnippet[];
  /** Optional overall title displayed on the left side of the header */
  title?: string;
  /** Default language to select if no preference is saved */
  defaultLanguage?: string;
  /** Custom wrapper class names */
  className?: string;
  /** Support for MDX children if provided */
  children?: React.ReactNode;
}

const STORAGE_KEY = "tessera_preferred_code_lang";
const SYNC_EVENT_NAME = "tessera-code-lang-change";

/** Normalize language aliases for cross-component synchronization */
function normalizeLang(lang: string): string {
  const l = lang.toLowerCase().trim();
  if (l === "js" || l === "javascript") return "javascript";
  if (l === "ts" || l === "typescript") return "typescript";
  if (l === "rs" || l === "rust") return "rust";
  if (l === "py" || l === "python") return "python";
  if (l === "sh" || l === "bash" || l === "shell" || l === "curl") return "curl";
  return l;
}

/** Pretty label for default language tags */
function getDisplayLabel(lang: string, customLabel?: string): string {
  if (customLabel) return customLabel;
  const norm = normalizeLang(lang);
  switch (norm) {
    case "curl":
      return "cURL";
    case "typescript":
      return "TypeScript";
    case "javascript":
      return "JavaScript";
    case "rust":
      return "Rust SDK";
    case "python":
      return "Python";
    case "json":
      return "JSON";
    default:
      return lang.charAt(0).toUpperCase() + lang.slice(1);
  }
}

/**
 * Production-grade responsive multi-tab code block component with:
 * - Cross-site synchronized language switching via localStorage & CustomEvents
 * - WAI-ARIA tablist/tab/tabpanel compliance with full keyboard navigation
 * - Copy-to-clipboard with accessible animated tooltips
 * - Dark mode optimized high-contrast styling
 */
export function MultiCodeBlock({
  snippets = [],
  title,
  defaultLanguage,
  className = "",
  children,
}: MultiCodeBlockProps) {
  const instanceId = useId().replace(/:/g, "_");
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);

  // If no explicit snippets provided but children exist, parse or fallback
  const items: CodeSnippet[] = useMemo(
    () => (snippets.length > 0 ? snippets : []),
    [snippets]
  );

  // Determine initial selected tab index
  const [selectedIndex, setSelectedIndex] = useState<number>(() => {
    if (items.length === 0) return 0;
    if (typeof window !== "undefined") {
      try {
        const stored = localStorage.getItem(STORAGE_KEY);
        if (stored) {
          const matchIdx = items.findIndex(
            (s) => normalizeLang(s.language) === normalizeLang(stored)
          );
          if (matchIdx !== -1) return matchIdx;
        }
      } catch {
        /* localStorage unavailable */
      }
    }
    if (defaultLanguage) {
      const defIdx = items.findIndex(
        (s) => normalizeLang(s.language) === normalizeLang(defaultLanguage)
      );
      if (defIdx !== -1) return defIdx;
    }
    return 0;
  });

  const [copied, setCopied] = useState(false);
  const copyTimeoutRef = useRef<NodeJS.Timeout | null>(null);

  // Sync with global language changes
  const updateLanguage = useCallback(
    (newLang: string) => {
      const target = normalizeLang(newLang);
      const matchIdx = items.findIndex(
        (s) => normalizeLang(s.language) === target
      );
      if (matchIdx !== -1) {
        setSelectedIndex(matchIdx);
      }
    },
    [items]
  );

  useEffect(() => {
    function handleStorage(e: StorageEvent) {
      if (e.key === STORAGE_KEY && e.newValue) {
        updateLanguage(e.newValue);
      }
    }

    function handleCustomSync(e: Event) {
      const customEvent = e as CustomEvent<string>;
      if (customEvent.detail) {
        updateLanguage(customEvent.detail);
      }
    }

    window.addEventListener("storage", handleStorage);
    window.addEventListener(SYNC_EVENT_NAME, handleCustomSync);

    // Initial check from localStorage after mount
    try {
      const stored = localStorage.getItem(STORAGE_KEY);
      if (stored) {
        updateLanguage(stored);
      }
    } catch {
      /* ignore */
    }

    return () => {
      window.removeEventListener("storage", handleStorage);
      window.removeEventListener(SYNC_EVENT_NAME, handleCustomSync);
      if (copyTimeoutRef.current) {
        clearTimeout(copyTimeoutRef.current);
      }
    };
  }, [updateLanguage]);

  function handleSelectTab(index: number, broadcast = true) {
    setSelectedIndex(index);
    const selectedItem = items[index];
    if (selectedItem && broadcast && typeof window !== "undefined") {
      const norm = normalizeLang(selectedItem.language);
      try {
        localStorage.setItem(STORAGE_KEY, norm);
      } catch {
        /* ignore */
      }
      window.dispatchEvent(
        new CustomEvent(SYNC_EVENT_NAME, { detail: norm })
      );
    }
  }

  // Keyboard accessibility: Left, Right, Home, End navigation between tabs
  function handleKeyDown(e: React.KeyboardEvent<HTMLButtonElement>, index: number) {
    let nextIndex = index;
    if (e.key === "ArrowRight") {
      e.preventDefault();
      nextIndex = (index + 1) % items.length;
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      nextIndex = (index - 1 + items.length) % items.length;
    } else if (e.key === "Home") {
      e.preventDefault();
      nextIndex = 0;
    } else if (e.key === "End") {
      e.preventDefault();
      nextIndex = items.length - 1;
    } else {
      return;
    }

    handleSelectTab(nextIndex, true);
    tabRefs.current[nextIndex]?.focus();
  }

  async function handleCopy() {
    const activeSnippet = items[selectedIndex];
    const textToCopy = activeSnippet ? activeSnippet.code : "";
    if (!textToCopy) return;

    try {
      await navigator.clipboard.writeText(textToCopy);
      setCopied(true);
      if (copyTimeoutRef.current) clearTimeout(copyTimeoutRef.current);
      copyTimeoutRef.current = setTimeout(() => {
        setCopied(false);
      }, 2000);
    } catch {
      /* clipboard write failed */
    }
  }

  const activeSnippet = items[selectedIndex] || items[0];

  return (
    <div
      className={`my-6 overflow-hidden rounded-xl border border-white/10 bg-[#0a0c11] shadow-xl ${className}`}
      data-testid="multi-code-block"
    >
      {/* Header bar with title, tabs, and copy action */}
      <div className="flex flex-wrap items-center justify-between border-b border-white/10 bg-base-950/60 px-3 py-1.5 sm:px-4">
        <div className="flex items-center gap-3 overflow-x-auto py-1">
          {title && (
            <span className="font-mono text-xs font-semibold text-base-300 mr-2 shrink-0">
              {title}
            </span>
          )}

          {items.length > 0 && (
            <div
              role="tablist"
              aria-label={title ? `${title} language examples` : "Code language tabs"}
              className="flex items-center gap-1"
            >
              {items.map((snippet, idx) => {
                const isSelected = idx === selectedIndex;
                const tabId = `tab-${instanceId}-${idx}`;
                const panelId = `panel-${instanceId}-${idx}`;
                const label = getDisplayLabel(snippet.language, snippet.label);

                return (
                  <button
                    key={`${snippet.language}-${idx}`}
                    ref={(el) => {
                      tabRefs.current[idx] = el;
                    }}
                    id={tabId}
                    type="button"
                    role="tab"
                    aria-selected={isSelected}
                    aria-controls={panelId}
                    tabIndex={isSelected ? 0 : -1}
                    onClick={() => handleSelectTab(idx, true)}
                    onKeyDown={(e) => handleKeyDown(e, idx)}
                    className={`rounded-md px-3 py-1.5 text-xs font-medium transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 ${
                      isSelected
                        ? "bg-brand-500/20 text-brand-300 font-semibold border border-brand-500/30"
                        : "text-base-300 hover:bg-white/5 hover:text-base-100 border border-transparent"
                    }`}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          )}
        </div>

        {/* Copy Button with interactive tooltip */}
        <div className="flex items-center gap-2 py-1">
          {activeSnippet?.title && (
            <span className="hidden sm:inline font-mono text-xs text-base-300">
              {activeSnippet.title}
            </span>
          )}
          <button
            type="button"
            onClick={handleCopy}
            aria-label={copied ? "Code copied to clipboard" : "Copy code snippet to clipboard"}
            className="flex items-center gap-1.5 rounded-lg border border-white/10 bg-base-900/60 px-2.5 py-1 text-xs font-medium text-base-200 transition-colors hover:border-brand-500/40 hover:bg-base-800 hover:text-base-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
          >
            {copied ? (
              <>
                <svg
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2.5"
                  className="text-brand-400 animate-in fade-in"
                  aria-hidden="true"
                >
                  <path d="M20 6 9 17l-5-5" strokeLinecap="round" strokeLinejoin="round" />
                </svg>
                <span className="text-brand-300 font-semibold" aria-live="polite">
                  Copied!
                </span>
              </>
            ) : (
              <>
                <svg
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  className="text-base-300"
                  aria-hidden="true"
                >
                  <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
                  <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
                </svg>
                <span>Copy</span>
              </>
            )}
          </button>
        </div>
      </div>

      {/* Code Display Area */}
      {items.length > 0 ? (
        items.map((snippet, idx) => {
          const isSelected = idx === selectedIndex;
          const tabId = `tab-${instanceId}-${idx}`;
          const panelId = `panel-${instanceId}-${idx}`;

          return (
            <div
              key={`${snippet.language}-${idx}`}
              id={panelId}
              role="tabpanel"
              aria-labelledby={tabId}
              hidden={!isSelected}
              className={`transition-opacity duration-150 ${isSelected ? "block opacity-100" : "hidden opacity-0"}`}
            >
              <pre className="overflow-x-auto p-4 text-sm leading-relaxed bg-[#0a0c11]">
                <code className={`font-mono text-base-100 language-${snippet.language}`}>
                  {snippet.code}
                </code>
              </pre>
            </div>
          );
        })
      ) : (
        <pre className="overflow-x-auto p-4 text-sm leading-relaxed bg-[#0a0c11]">
          <code className="font-mono text-base-100">{children}</code>
        </pre>
      )}
    </div>
  );
}

export default MultiCodeBlock;
