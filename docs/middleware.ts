import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";
import { LOCALES, DEFAULT_LOCALE, isValidLocale, getLocaleFromPath } from "@/lib/i18n";

const PUBLIC_FILE = /\.(.*)$/;
const LOCALE_COOKIE = "NEXT_LOCALE";

export function middleware(request: NextRequest) {
  const { pathname } = request.nextUrl;

  if (
    pathname.startsWith("/_next") ||
    pathname.startsWith("/api") ||
    pathname.startsWith("/static") ||
    PUBLIC_FILE.test(pathname)
  ) {
    return NextResponse.next();
  }

  const pathnameLocale = getLocaleFromPath(pathname);

  if (pathnameLocale) {
    const response = NextResponse.next();
    response.cookies.set(LOCALE_COOKIE, pathnameLocale, { path: "/" });
    return response;
  }

  const cookieLocale = request.cookies.get(LOCALE_COOKIE)?.value;
  const preferredLocale =
    cookieLocale && isValidLocale(cookieLocale) ? cookieLocale : DEFAULT_LOCALE;

  const acceptLanguage = request.headers.get("accept-language");
  const detectedLocale = acceptLanguage
    ? detectLocaleFromAcceptLanguage(acceptLanguage)
    : null;

  const locale = detectedLocale && isValidLocale(detectedLocale) ? detectedLocale : preferredLocale;

  const newUrl = new URL(`/${locale}${pathname}`, request.url);
  newUrl.search = request.nextUrl.search;

  const response = NextResponse.redirect(newUrl);
  response.cookies.set(LOCALE_COOKIE, locale, { path: "/" });

  return response;
}

function detectLocaleFromAcceptLanguage(acceptLanguage: string): string | null {
  const languages = acceptLanguage
    .split(",")
    .map((lang) => lang.split(";")[0].trim().toLowerCase())
    .map((lang) => lang.split("-")[0]);

  for (const lang of languages) {
    if (LOCALES.includes(lang as (typeof LOCALES)[number])) {
      return lang;
    }
  }

  return null;
}

export const config = {
  matcher: [
    "/((?!_next|api|static|_not-found|404|.*\\..*).*)",
  ],
};