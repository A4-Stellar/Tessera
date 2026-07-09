"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { FLAT_NAV } from "./nav";

/** Previous / next page links derived from the flattened nav order. */
export function PrevNext() {
  const pathname = usePathname();
  const idx = FLAT_NAV.findIndex((i) => i.href === pathname);
  if (idx === -1) return null;
  const prev = idx > 0 ? FLAT_NAV[idx - 1] : null;
  const next = idx < FLAT_NAV.length - 1 ? FLAT_NAV[idx + 1] : null;

  return (
    <div className="mt-16 grid grid-cols-1 gap-4 border-t border-white/5 pt-8 sm:grid-cols-2">
      {prev ? (
        <Link href={prev.href} className="group rounded-xl border border-white/10 p-4 transition-colors hover:border-brand-500/40">
          <span className="text-xs text-base-300">← Previous</span>
          <p className="mt-1 font-medium text-base-100 group-hover:text-brand-300">{prev.title}</p>
        </Link>
      ) : (
        <span />
      )}
      {next && (
        <Link href={next.href} className="group rounded-xl border border-white/10 p-4 text-right transition-colors hover:border-brand-500/40">
          <span className="text-xs text-base-300">Next →</span>
          <p className="mt-1 font-medium text-base-100 group-hover:text-brand-300">{next.title}</p>
        </Link>
      )}
    </div>
  );
}

export default PrevNext;
