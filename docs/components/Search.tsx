"use client";

import { useState, useRef, useEffect } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { initSearch, search, type SearchDocument } from "@/lib/search";

/** Client-side documentation search with keyboard shortcuts. */
export function Search() {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchDocument[]>([]);
  const [searchReady, setSearchReady] = useState(false);
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const inputRef = useRef<HTMLInputElement>(null);
  const router = useRouter();

  useEffect(() => {
    let mounted = true;
    initSearch().then(() => {
      if (mounted) setSearchReady(true);
    });
    return () => {
      mounted = false;
    };
  }, []);

  useEffect(() => {
    setActiveIndex(-1);
    if (query.trim() && searchReady) {
      setResults(search(query.trim()));
      setIsOpen(true);
    } else {
      setResults([]);
      setIsOpen(false);
    }
  }, [query, searchReady]);

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        inputRef.current?.focus();
      }
      if (e.key === "Escape") {
        setIsOpen(false);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  function selectResult(result: SearchDocument) {
    setQuery("");
    setIsOpen(false);
    setActiveIndex(-1);
    router.push(result.route);
  }

  function handleInputKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (!isOpen || results.length === 0) return;

    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((i) => (i + 1) % results.length);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((i) => (i <= 0 ? results.length - 1 : i - 1));
    } else if (e.key === "Enter") {
      if (activeIndex >= 0 && activeIndex < results.length) {
        e.preventDefault();
        selectResult(results[activeIndex]);
      }
    } else if (e.key === "Escape") {
      setIsOpen(false);
    }
  }

  const showEmptyState = isOpen && query.trim().length > 0 && results.length === 0;

  return (
    <div className="mb-6">
      <div className="relative">
        <label htmlFor="doc-search-input" className="sr-only">
          Search documentation
        </label>
        <input
          ref={inputRef}
          id="doc-search-input"
          type="search"
          placeholder="Search docs... (⌘K)"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActiveIndex(-1);
          }}
          onFocus={() => query && setIsOpen(true)}
          onKeyDown={handleInputKeyDown}
          role="combobox"
          aria-expanded={isOpen}
          aria-controls="search-results-list"
          aria-activedescendant={activeIndex >= 0 ? `search-result-${activeIndex}` : undefined}
          aria-autocomplete="list"
          aria-label="Search documentation"
          className="w-full rounded-lg border border-base-300/40 bg-base-900/60 px-3 py-2 text-sm text-base-100 placeholder-base-300 transition-colors focus:border-brand-500/70 focus:outline-none focus:ring-2 focus:ring-brand-500/30"
        />
        {isOpen && results.length > 0 && (
          <div className="absolute top-full z-50 mt-2 w-full overflow-hidden rounded-lg border border-base-300/30 bg-base-900 shadow-xl">
            <div
              id="search-results-list"
              role="listbox"
              aria-label="Search results"
              className="max-h-96 overflow-y-auto py-1"
            >
              {results.map((result, index) => (
                <div
                  key={result.route}
                  id={`search-result-${index}`}
                  role="option"
                  aria-selected={index === activeIndex}
                >
                  <Link
                    href={result.route}
                    onClick={() => {
                      setQuery("");
                      setIsOpen(false);
                    }}
                    onMouseEnter={() => setActiveIndex(index)}
                    className={`block px-3 py-2 text-sm transition-colors hover:bg-base-800 focus:bg-base-800 focus:outline-none ${
                      index === activeIndex ? "bg-base-800 ring-1 ring-inset ring-brand-500/30" : ""
                    }`}
                  >
                    <div className="font-medium text-base-100">{result.title}</div>
                    <div className="text-xs text-base-300">{result.headers}</div>
                    {result.content && (
                      <div className="mt-1 line-clamp-1 text-xs text-base-200/80">
                        {result.content}
                      </div>
                    )}
                  </Link>
                </div>
              ))}
            </div>
          </div>
        )}
        {showEmptyState && (
          <div className="absolute top-full z-50 mt-2 w-full overflow-hidden rounded-lg border border-base-300/30 bg-base-900 shadow-xl">
            <p className="px-3 py-4 text-center text-sm text-base-300" role="status">
              No results for &ldquo;{query}&rdquo;.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
