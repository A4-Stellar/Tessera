import { notFound } from "next/navigation";
import { LOCALES, type Locale } from "@/lib/i18n";

interface NotFoundPageProps {
  params: Promise<{ lang?: string }> | undefined;
}

export async function generateStaticParams() {
  return LOCALES.map((locale) => ({ lang: locale }));
}

export default async function NotFound({ params }: NotFoundPageProps) {
  if (!params) {
    notFound();
    return;
  }
  
  const resolvedParams = await params;
  const lang = resolvedParams?.lang;
  
  if (!lang || !LOCALES.includes(lang as Locale)) {
    notFound();
    return;
  }

  notFound();
}