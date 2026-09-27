"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { getFlatNav } from "./nav";
import { getLocaleFromPath } from "@/lib/i18n";

/** Previous / next page links derived from the flattened nav order. */
export function PrevNext() {
  const pathname = usePathname();
  const locale = getLocaleFromPath(pathname) || "en";
  const flatNav = getFlatNav(locale);
  const idx = flatNav.findIndex((i) => i.href === pathname);
  if (idx === -1) return null;
  const prev = idx > 0 ? flatNav[idx - 1] : null;
  const next = idx < flatNav.length - 1 ? flatNav[idx + 1] : null;

  return (
    <nav aria-label="Page pagination" className="mt-16 grid grid-cols-1 gap-4 border-t border-white/5 pt-8 sm:grid-cols-2">
      {prev ? (
        <Link
          href={prev.href}
          aria-label={`Previous page: ${prev.title}`}
          className="group rounded-xl border border-white/10 p-4 transition-colors hover:border-brand-500/40 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        >
          <span className="text-xs text-base-300">← Previous</span>
          <p className="mt-1 font-medium text-base-100 group-hover:text-brand-300">{prev.title}</p>
        </Link>
      ) : (
        <span />
      )}
      {next && (
        <Link
          href={next.href}
          aria-label={`Next page: ${next.title}`}
          className="group rounded-xl border border-white/10 p-4 text-right transition-colors hover:border-brand-500/40 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        >
          <span className="text-xs text-base-300">Next →</span>
          <p className="mt-1 font-medium text-base-100 group-hover:text-brand-300">{next.title}</p>
        </Link>
      )}
    </nav>
  );
}

export default PrevNext;