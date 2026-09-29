import type { Metadata, Viewport } from "next";
import { Suspense } from "react";
import "./globals.css";
import { DocHeader } from "@/components/DocHeader";
import { ServiceWorkerRegister } from "@/components/ServiceWorkerRegister";
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
  manifest: "/manifest.webmanifest",
  appleWebApp: {
    capable: true,
    statusBarStyle: "black-translucent",
    title: "Tessera Docs",
  },
  icons: {
    icon: [
      { url: "/icons/icon-192.png", sizes: "192x192", type: "image/png" },
      { url: "/icons/icon-512.png", sizes: "512x512", type: "image/png" },
    ],
    apple: [{ url: "/icons/icon-192.png", sizes: "192x192", type: "image/png" }],
  },
};

export const viewport: Viewport = {
  themeColor: "#10b981",
  width: "device-width",
  initialScale: 1,
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
        <Suspense fallback={<header className="sticky top-0 z-40 border-b border-white/5 bg-base-950/80 backdrop-blur-xl h-16" />}>
          <DocHeader />
        </Suspense>
        {children}
        <ServiceWorkerRegister />
      </body>
    </html>
  );
}
