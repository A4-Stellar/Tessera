export const LOCALES = ["en", "es", "fr", "zh", "ja", "ar", "pt"] as const;
export const DEFAULT_LOCALE = "en" as const;
export const RTL_LOCALES = ["ar"] as const;

export type Locale = (typeof LOCALES)[number];
export type RtlLocale = (typeof RTL_LOCALES)[number];

export const LOCALE_NAMES: Record<Locale, string> = {
  en: "English",
  es: "Español",
  fr: "Français",
  zh: "中文",
  ja: "日本語",
  ar: "العربية",
  pt: "Português",
};

export const LOCALE_NATIVE_NAMES: Record<Locale, string> = {
  en: "English",
  es: "Español",
  fr: "Français",
  zh: "中文",
  ja: "日本語",
  ar: "العربية",
  pt: "Português",
};

export function isValidLocale(locale: string): locale is Locale {
  return LOCALES.includes(locale as Locale);
}

export function isRtlLocale(locale: string): locale is RtlLocale {
  return RTL_LOCALES.includes(locale as RtlLocale);
}

export function getLocaleDirection(locale: string): "ltr" | "rtl" {
  return isRtlLocale(locale) ? "rtl" : "ltr";
}

export function getLocaleFromPath(pathname: string): Locale | null {
  const segments = pathname.split("/").filter(Boolean);
  if (segments.length === 0) return null;
  const firstSegment = segments[0];
  return isValidLocale(firstSegment) ? firstSegment : null;
}

export function removeLocaleFromPath(pathname: string, locale: Locale): string {
  const prefix = `/${locale}`;
  if (pathname === prefix || pathname.startsWith(`${prefix}/`)) {
    return pathname.slice(prefix.length) || "/";
  }
  return pathname;
}

export function addLocaleToPath(pathname: string, locale: Locale): string {
  if (pathname === "/") {
    return `/${locale}`;
  }
  return `/${locale}${pathname}`;
}

export function getDefaultLocale(): Locale {
  return DEFAULT_LOCALE;
}

export function getFallbackLocale(locale: Locale): Locale {
  void locale;
  return DEFAULT_LOCALE;
}

export function getLocalizedPath(pathname: string, targetLocale: Locale): string {
  const currentLocale = getLocaleFromPath(pathname);
  const pathWithoutLocale = currentLocale
    ? removeLocaleFromPath(pathname, currentLocale)
    : pathname;
  return addLocaleToPath(pathWithoutLocale, targetLocale);
}
