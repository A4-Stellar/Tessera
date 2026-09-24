"use client";

import Link from "next/link";
import { useState, useEffect, useRef } from "react";
import { Sidebar } from "./Sidebar";

const REPO_URL = "https://github.com/A4-Stellar/Tessera";

/** Top navigation bar with an accessible mobile-only slide-in sidebar drawer. */
export function DocHeader() {
  const [open, setOpen] = useState(false);
  const toggleButtonRef = useRef<HTMLButtonElement>(null);
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const drawerRef = useRef<HTMLDivElement>(null);

  // Close drawer on Escape and trap focus
  useEffect(() => {
    if (!open) return;

    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setOpen(false);
        toggleButtonRef.current?.focus();
        return;
      }

      if (e.key === "Tab" && drawerRef.current) {
        const focusableElements = drawerRef.current.querySelectorAll<HTMLElement>(
          'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
        );
        if (focusableElements.length === 0) return;

        const firstElement = focusableElements[0];
        const lastElement = focusableElements[focusableElements.length - 1];

        if (e.shiftKey) {
          if (document.activeElement === firstElement) {
            e.preventDefault();
            lastElement.focus();
          }
        } else {
          if (document.activeElement === lastElement) {
            e.preventDefault();
            firstElement.focus();
          }
        }
      }
    }

    document.addEventListener("keydown", handleKeyDown);
    // Focus the close button when opened
    closeButtonRef.current?.focus();

    return () => {
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [open]);

  function closeDrawer() {
    setOpen(false);
    toggleButtonRef.current?.focus();
  }

  return (
    <>
      <header className="sticky top-0 z-40 border-b border-white/5 bg-base-950/80 backdrop-blur-xl">
        <div className="mx-auto flex h-16 max-w-screen-2xl items-center justify-between gap-4 px-4 sm:px-6">
          <div className="flex items-center gap-3">
            <button
              ref={toggleButtonRef}
              type="button"
              className="rounded-lg p-2 text-base-200 hover:bg-white/5 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 lg:hidden"
              onClick={() => setOpen(true)}
              aria-expanded={open}
              aria-controls="mobile-navigation-drawer"
              aria-label="Open navigation menu"
            >
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
                <path d="M3 12h18M3 6h18M3 18h18" strokeLinecap="round" />
              </svg>
            </button>
            <Link href="/" className="flex items-center gap-2.5 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded-lg">
              <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-gradient-to-br from-brand-400 to-brand-600 text-base-950" aria-hidden="true">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2">
                  <path d="m12 2 8 4.5v9L12 20l-8-4.5v-9L12 2Z" strokeLinejoin="round" />
                  <path d="M12 8v8M8 10v4M16 10v4" strokeLinecap="round" />
                </svg>
              </span>
              <span className="text-sm font-bold tracking-tight text-base-50">
                Tessera<span className="text-brand-400"> Docs</span>
              </span>
            </Link>
          </div>

          <div className="flex items-center gap-1 text-sm">
            <Link href="/docs/getting-started" className="hidden rounded-lg px-3 py-2 text-base-200/90 hover:text-base-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 sm:block">
              Docs
            </Link>
            <a
              href={REPO_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="rounded-lg px-3 py-2 text-base-200/90 hover:text-base-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 sm:block"
              aria-label="Tessera GitHub Repository (opens in new tab)"
            >
              GitHub ↗
            </a>
          </div>
        </div>
      </header>

      {/* Mobile drawer with focus trap and ARIA dialog */}
      {open && (
        <div
          id="mobile-navigation-drawer"
          role="dialog"
          aria-modal="true"
          aria-label="Mobile navigation"
          className="fixed inset-0 z-50 lg:hidden"
        >
          <button
            type="button"
            className="absolute inset-0 h-full w-full bg-black/60 cursor-default"
            onClick={closeDrawer}
            aria-label="Close mobile navigation overlay"
            tabIndex={-1}
          />
          <div
            ref={drawerRef}
            className="absolute left-0 top-0 h-full w-72 overflow-y-auto border-r border-white/10 bg-base-900 p-5 shadow-2xl"
          >
            <div className="mb-5 flex items-center justify-between">
              <span className="text-sm font-semibold text-base-100">Navigation</span>
              <button
                ref={closeButtonRef}
                type="button"
                onClick={closeDrawer}
                aria-label="Close navigation menu"
                className="rounded-lg p-1.5 text-base-300 hover:bg-white/5 hover:text-base-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
              >
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
                  <path d="M18 6 6 18M6 6l12 12" strokeLinecap="round" />
                </svg>
              </button>
            </div>
            <Sidebar onNavigate={closeDrawer} />
          </div>
        </div>
      )}
    </>
  );
}

export default DocHeader;
