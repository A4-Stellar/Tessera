import { redirect } from "next/navigation";
import { DEFAULT_LOCALE } from "@/lib/i18n";

interface RootDocsPageProps {
  params: Promise<{ lang: string }>;
}

export default async function RootDocsPage({ params }: RootDocsPageProps) {
  const { lang } = await params;
  redirect(`/${lang}/docs/getting-started`);
}