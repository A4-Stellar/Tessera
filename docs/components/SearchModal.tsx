"use client";

import { useEffect, useState, useRef } from 'react';
import { useRouter } from 'next/navigation';
import { initSearch, search, SearchDocument } from '../lib/search';

export default function SearchModal() {
  const [isOpen, setIsOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<SearchDocument[]>([]);
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const router = useRouter();

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        setIsOpen((prev) => !prev);
      }
      if (e.key === 'Escape') {
        setIsOpen(false);
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  useEffect(() => {
    if (isOpen) {
      initSearch().then(() => {
        inputRef.current?.focus();
      });
    } else {
      setQuery('');
      setResults([]);
      setActiveIndex(0);
    }
  }, [isOpen]);

  useEffect(() => {
    if (query) {
      setResults(search(query));
      setActiveIndex(0);
    } else {
      setResults([]);
    }
  }, [query]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (results.length > 0) {
        setActiveIndex((prev) => (prev + 1) % results.length);
      }
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (results.length > 0) {
        setActiveIndex((prev) => (prev - 1 + results.length) % results.length);
      }
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (results.length > 0) {
        handleSelect(results[activeIndex]);
      }
    }
  };

  const handleSelect = (doc: SearchDocument) => {
    setIsOpen(false);
    router.push(doc.route);
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-16 sm:pt-24">
      <button
        type="button"
        className="absolute inset-0 h-full w-full cursor-default bg-black/50"
        aria-label="Close search"
        onClick={() => setIsOpen(false)}
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Search documentation"
        className="w-full max-w-xl bg-white dark:bg-gray-900 rounded-xl shadow-2xl overflow-hidden"
      >
        <div className="p-4 border-b border-gray-200 dark:border-gray-800 flex items-center">
          <svg className="w-5 h-5 text-gray-400 mr-3" fill="none" stroke="currentColor" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
          </svg>
          <input
            ref={inputRef}
            type="text"
            className="w-full bg-transparent border-0 focus:ring-0 text-gray-900 dark:text-white placeholder-gray-400 outline-none"
            placeholder="Search documentation..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
          />
          <div className="flex items-center text-xs text-gray-400 font-semibold px-2 py-1 bg-gray-100 dark:bg-gray-800 rounded">
            ESC
          </div>
        </div>
        
        {results.length > 0 && (
          <ul className="max-h-96 overflow-y-auto p-2">
            {results.map((res, i) => (
              <li key={res.id}>
                <button
                  type="button"
                  className={`w-full p-3 text-left rounded-lg cursor-pointer flex flex-col gap-1 ${
                    i === activeIndex
                      ? 'bg-blue-50 dark:bg-blue-900/30'
                      : 'hover:bg-gray-50 dark:hover:bg-gray-800/50'
                  }`}
                  onClick={() => handleSelect(res)}
                  onMouseEnter={() => setActiveIndex(i)}
                >
                  <div className="font-medium text-gray-900 dark:text-white flex justify-between">
                    <span>{res.title}</span>
                    <span className="text-xs text-gray-400">{res.route}</span>
                  </div>
                  <div className="text-sm text-gray-500 dark:text-gray-400 truncate">
                    {res.headers ? res.headers : res.content.substring(0, 100) + "..."}
                  </div>
                </button>
              </li>
            ))}
          </ul>
        )}
        
        {query && results.length === 0 && (
          <div className="p-8 text-center text-gray-500 dark:text-gray-400">
            No results found for &ldquo;{query}&rdquo;
          </div>
        )}
      </div>
    </div>
  );
}
