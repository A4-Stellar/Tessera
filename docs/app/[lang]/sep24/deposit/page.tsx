"use client";

/**
 * SEP-24 interactive deposit landing page.
 *
 * The anchor's `/sep24/transactions/deposit/interactive` response points here
 * with `?transaction_id=…&token=…`. `useSearchParams` requires a Suspense
 * boundary under the App Router, so the inner component is wrapped below.
 */

import { Suspense } from "react";
import { useParams, useSearchParams } from "next/navigation";
import { DepositForm } from "@/components/sep24/DepositForm";
import { DEFAULT_LOCALE, isValidLocale } from "@/lib/i18n";

function DepositRoute({ lang }: { lang: string }) {
  const searchParams = useSearchParams();
  const transactionId = searchParams.get("transaction_id") ?? "";
  const token = searchParams.get("token") ?? "";

  return (
    <main id="main-content" className="mx-auto w-full max-w-5xl px-4 py-12">
      <DepositForm transactionId={transactionId} token={token} lang={lang} />
    </main>
  );
}

export default function Sep24DepositPage() {
  const params = useParams<{ lang: string }>();
  const rawLang = Array.isArray(params.lang) ? params.lang[0] : params.lang;
  const lang = isValidLocale(rawLang ?? "") ? rawLang! : DEFAULT_LOCALE;

  return (
    <Suspense
      fallback={
        <main id="main-content" className="mx-auto w-full max-w-5xl px-4 py-12 text-sm text-base-400">
          Loading deposit…
        </main>
      }
    >
      <DepositRoute lang={lang} />
    </Suspense>
  );
}
