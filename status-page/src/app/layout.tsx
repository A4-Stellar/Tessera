import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Tessera Status",
  description:
    "Real-time operational status for Tessera API, Soroban Testnet RPC, and documentation.",
  openGraph: {
    title: "Tessera Status",
    description: "Real-time status and incident history for Tessera infrastructure.",
    type: "website",
  },
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className="dark">
      <head>
        {/* RSS auto-discovery */}
        <link
          rel="alternate"
          type="application/rss+xml"
          title="Tessera Status — Incident Feed"
          href="/api/rss"
        />
      </head>
      <body className="antialiased min-h-screen bg-gray-950 text-gray-200">
        {children}
      </body>
    </html>
  );
}
