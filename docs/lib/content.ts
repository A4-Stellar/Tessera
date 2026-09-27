import fs from "fs";
import path from "path";
import matter from "gray-matter";

const CONTENT_ROOT = path.join(process.cwd(), "content");
const LOCALES = ["en", "es", "pt", "ja", "fr"] as const;
const DEFAULT_LOCALE = "en";

export type Locale = (typeof LOCALES)[number];

export interface ContentFile {
  slug: string;
  locale: Locale;
  frontMatter: Record<string, unknown>;
  content: string;
  filePath: string;
}

export function getContentPath(locale: Locale, ...segments: string[]): string {
  return path.join(CONTENT_ROOT, locale, ...segments);
}

export function getContentFilePath(locale: Locale, slug: string): string {
  const normalizedSlug = slug.replace(/\//g, "-");
  return getContentPath(locale, "docs", `${normalizedSlug}.mdx`);
}

export function getLocaleContentDir(locale: Locale): string {
  return path.join(CONTENT_ROOT, locale, "docs");
}

export function readContentFile(filePath: string): ContentFile | null {
  if (!fs.existsSync(filePath)) return null;

  const fileContent = fs.readFileSync(filePath, "utf-8");
  const { data: frontMatter, content } = matter(fileContent);
  const relativePath = path.relative(path.join(CONTENT_ROOT, "en", "docs"), filePath);
  const slug = relativePath.replace(/\.mdx$/, "").replace(/\\/g, "/");

  const locale = filePath.split(path.sep).includes("en") ? "en" :
                 filePath.split(path.sep).includes("es") ? "es" :
                 filePath.split(path.sep).includes("pt") ? "pt" :
                 filePath.split(path.sep).includes("ja") ? "ja" :
                 filePath.split(path.sep).includes("fr") ? "fr" : "en";

  return {
    slug,
    locale: locale as Locale,
    frontMatter,
    content,
    filePath,
  };
}

export function findContentFile(slug: string, locale: Locale): ContentFile | null {
  const localePath = getContentFilePath(locale, slug);
  const file = readContentFile(localePath);
  if (file) return file;

  if (locale !== DEFAULT_LOCALE) {
    const fallbackPath = getContentFilePath(DEFAULT_LOCALE, slug);
    return readContentFile(fallbackPath);
  }

  return null;
}

export function getAllContentFiles(locale: Locale): ContentFile[] {
  const dir = getLocaleContentDir(locale);
  if (!fs.existsSync(dir)) return [];

  const files: ContentFile[] = [];

  function walk(dirPath: string) {
    const entries = fs.readdirSync(dirPath, { withFileTypes: true });
    for (const entry of entries) {
      const fullPath = path.join(dirPath, entry.name);
      if (entry.isDirectory()) {
        walk(fullPath);
      } else if (entry.name.endsWith(".mdx")) {
        const file = readContentFile(fullPath);
        if (file) files.push(file);
      }
    }
  }

  walk(dir);
  return files;
}

export function getAvailableLocalesForSlug(slug: string): Locale[] {
  return LOCALES.filter((locale) => {
    const filePath = getContentFilePath(locale, slug);
    return fs.existsSync(filePath);
  });
}

export function slugToPath(slug: string): string {
  return `/docs/${slug}`;
}

export function pathToSlug(path: string): string {
  return path.replace(/^\/docs\//, "").replace(/\/$/, "");
}