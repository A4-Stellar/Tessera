import type { Metadata } from "next";
import { Suspense } from "react";
import { notFound } from "next/navigation";
import { LOCALES, type Locale, LOCALE_NAMES } from "@/lib/i18n";
import { DocHeader } from "@/components/DocHeader";
import { SITE_URL } from "@/lib/site";

interface LangLayoutProps {
  children: React.ReactNode;
  params: Promise<{ lang: string }>;
}

const description =
  "Documentation for Tessera: Soroban contracts, the indexing REST API, and the web app for tokenizing real-world assets with on-chain compliance.";

export async function generateMetadata({ params }: LangLayoutProps): Promise<Metadata> {
  const { lang } = await params;
  const locale = lang as Locale;

  if (!LOCALES.includes(locale)) {
    notFound();
  }

  return {
    metadataBase: new URL(SITE_URL),
    title: {
      default: "Tessera Docs",
      template: "%s · Tessera Docs",
    },
    description,
    openGraph: {
      title: {
        default: "Tessera Docs",
        template: "%s · Tessera Docs",
      },
      description,
      type: "website",
      locale: locale,
      alternateLocale: LOCALES.filter((l) => l !== locale),
      url: `${SITE_URL}/${locale}`,
    },
    twitter: {
      card: "summary_large_image",
      title: {
        default: "Tessera Docs",
        template: "%s · Tessera Docs",
      },
      description,
    },
    alternates: {
      languages: Object.fromEntries(
        LOCALES.map((l) => [l, `${SITE_URL}/${l}`])
      ),
    },
  };
}

export async function generateStaticParams() {
  return LOCALES.map((locale) => ({ lang: locale }));
}

export default async function LangLayout({ children, params }: LangLayoutProps) {
  const { lang } = await params;
  const locale = lang as Locale;

  if (!LOCALES.includes(locale)) {
    notFound();
  }

  return (
    <html lang={locale} className="dark">
      <head>
        <link rel="alternate" hrefLang="x-default" href={SITE_URL} />
        {LOCALES.map((l) => (
          <link
            key={l}
            rel="alternate"
            hrefLang={l}
            href={`${SITE_URL}/${l}`}
          />
        ))}
      </head>
      <body className="min-h-screen">
        <a
          href="#main-content"
          className="sr-only focus:not-sr-only focus:fixed focus:top-4 focus:left-4 focus:z-50 focus:rounded-lg focus:bg-brand-500 focus:px-4 focus:py-2.5 focus:text-sm focus:font-bold focus:text-base-950 focus:shadow-2xl focus:outline-none focus:ring-2 focus:ring-brand-300 focus:ring-offset-2 focus:ring-offset-base-950"
        >
          Skip to main content
        </a>
        <Suspense fallback={<header className="sticky top-0 z-40 border-b border-white/5 bg-base-950/80 backdrop-blur-xl h-16" />}>
          <DocHeader />
        </Suspense>
        {children}
      </body>
    </html>
  );
}