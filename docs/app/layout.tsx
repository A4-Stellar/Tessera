import type { Metadata } from "next";
import "./globals.css";
import { DocHeader } from "@/components/DocHeader";
import { SITE_URL } from "@/lib/site";

const description =
  "Documentation for Tessera: Soroban contracts, the indexing REST API, and the web app for tokenizing real-world assets with on-chain compliance.";

export const metadata: Metadata = {
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
    url: SITE_URL,
  },
  twitter: {
    card: "summary_large_image",
    title: {
      default: "Tessera Docs",
      template: "%s · Tessera Docs",
    },
    description,
  },
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className="dark">
      <body className="min-h-screen">
        <a
          href="#main-content"
          className="sr-only focus:not-sr-only focus:fixed focus:top-4 focus:left-4 focus:z-50 focus:rounded-lg focus:bg-brand-500 focus:px-4 focus:py-2.5 focus:text-sm focus:font-bold focus:text-base-950 focus:shadow-2xl focus:outline-none focus:ring-2 focus:ring-brand-300 focus:ring-offset-2 focus:ring-offset-base-950"
        >
          Skip to main content
        </a>
        <DocHeader />
        {children}
      </body>
    </html>
  );
}
