import { LOCALES, type Locale } from "@/lib/i18n";

export interface NavItem {
  title: string;
  href: string;
}

export interface NavSection {
  title: string;
  items: NavItem[];
}

const baseNavSections = [
  {
    title: "Introduction",
    items: [
      { title: "Getting Started", href: "/docs/getting-started" },
      { title: "Architecture Overview", href: "/docs/architecture" },
    ],
  },
  {
    title: "Contract Reference",
    items: [
      { title: "Asset Token", href: "/docs/contracts/asset-token" },
      { title: "Compliance", href: "/docs/contracts/compliance" },
      { title: "Registry", href: "/docs/contracts/registry" },
      { title: "Dividend", href: "/docs/contracts/dividend" },
    ],
  },
  {
    title: "API Reference",
    items: [
      { title: "Overview", href: "/docs/api/overview" },
      { title: "Assets", href: "/docs/api/assets" },
      { title: "Holders", href: "/docs/api/holders" },
      { title: "Compliance", href: "/docs/api/compliance" },
      { title: "Dividends", href: "/docs/api/dividends" },
      { title: "Rate Limits & Caching", href: "/docs/api/rate-limits" },
    ],
  },
  {
    title: "Guides",
    items: [
      { title: "Compliance Guide", href: "/docs/compliance-guide" },
      { title: "Time & Ledgers", href: "/docs/time-and-ledgers" },
      { title: "Web App Guide", href: "/docs/web-app" },
      { title: "Issuer Wizard", href: "/docs/web-app#issuer-wizard" },
      { title: "Prospectus Viewer", href: "/docs/prospectus-viewer" },
      { title: "Integration", href: "/docs/integration" },
    ],
  },
] as const;

export function getNav(locale: Locale): NavSection[] {
  return baseNavSections.map((section) => ({
    ...section,
    items: section.items.map((item) => ({
      ...item,
      href: `/${locale}${item.href}`,
    })),
  }));
}

export function getFlatNav(locale: Locale): NavItem[] {
  return getNav(locale).flatMap((s) => s.items);
}

export const NAV = getNav("en");
export const FLAT_NAV = getFlatNav("en");