"use client";

import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { useState, useRef, useEffect } from "react";
import { LOCALES, LOCALE_NAMES, LOCALE_NATIVE_NAMES, type Locale, getLocalizedPath } from "@/lib/i18n";

export function LanguageSwitcher() {
  const pathname = usePathname();
  const { replace } = useRouter();
  const searchParams = useSearchParams();
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const currentLocale = (pathname.split("/")[1] as Locale) || "en";

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (!open) return;
      if (e.key === "Escape") {
        setOpen(false);
        buttonRef.current?.focus();
      }
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const handleClickOutside = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node) && e.target !== buttonRef.current) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, [open]);

  function handleLocaleChange(locale: Locale) {
    const newPath = getLocalizedPath(pathname, locale);
    const search = searchParams.toString();
    const url = search ? `${newPath}?${search}` : newPath;
    replace(url, { scroll: false });
    setOpen(false);
  }

  return (
    <div className="relative" ref={menuRef}>
      <button
        ref={buttonRef}
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-haspopup="listbox"
        aria-label="Select language"
        className="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-sm text-base-200/90 hover:text-base-100 hover:bg-white/5 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 transition-colors"
      >
        <span className="font-medium">{LOCALE_NATIVE_NAMES[currentLocale]}</span>
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
          <path d="M6 9l6 6 6-6" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>

      {open && (
        <ul
          role="listbox"
          aria-label="Available languages"
          className="absolute right-0 top-full mt-1.5 min-w-[140px] rounded-lg border border-white/10 bg-base-900 py-1.5 shadow-lg z-50 animate-fade-in"
        >
          {LOCALES.map((locale) => (
            <li key={locale}>
              <button
                role="option"
                aria-selected={locale === currentLocale}
                onClick={() => handleLocaleChange(locale)}
                className={`w-full px-3 py-2 text-sm text-left transition-colors focus:outline-none focus:bg-brand-500/20 ${
                  locale === currentLocale
                    ? "bg-brand-500/15 font-medium text-brand-300"
                    : "text-base-200/90 hover:bg-white/5 hover:text-base-100"
                }`}
              >
                <span className="flex items-center gap-2">
                  <span>{LOCALE_NATIVE_NAMES[locale]}</span>
                  <span className="text-xs text-base-300">({LOCALE_NAMES[locale]})</span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}

      <style jsx>{`
        @keyframes fade-in {
          from {
            opacity: 0;
            transform: translateY(-4px);
          }
          to {
            opacity: 1;
            transform: translateY(0);
          }
        }
        .animate-fade-in {
          animation: fade-in 150ms ease-out;
        }
      `}</style>
    </div>
  );
}

export default LanguageSwitcher;