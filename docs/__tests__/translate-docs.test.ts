/* eslint-disable @typescript-eslint/no-require-imports */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const core = require("../scripts/lib/mdx-translation.cjs");

const SOURCE = `export const metadata = {
  title: "Getting Started",
  description: "How the \`asset-token\` contract works with Stellar.",
  canonical: "/docs/getting-started",
};

import { Foo } from "@/components/Foo";

# Getting Started

The **Tessera** toolkit tokenizes assets. See the [compliance guide](/docs/compliance-guide) and https://stellar.org.

<CalloutBox variant="compliance" title="Why this matters">
Compliance is enforced **in the contract**, not in an app. Use \`transfer()\` with {props.amount} &amp; care.
</CalloutBox>

\`\`\`rust
// Comments in code are never translated.
pub fn transfer(env: Env) {}
\`\`\`

| Field | Meaning |
| --- | --- |
| \`id\` | Asset identifier |

{/* An MDX comment that must survive. */}

<ApiEndpoint method="GET" path="/v1/assets" />
`;

/** Upper-cases translatable text so the test can see exactly what was sent. */
const shout = async (templates: string[]) => templates.map((t) => t.replace(/[a-z]+/g, (w) => w.toUpperCase()));

describe("translateDocument", () => {
  it("translates prose and protected attribute text but never code, JSX or links", async () => {
    const sent: string[] = [];
    const translated: string = await core.translateDocument(SOURCE, {
      locale: "es",
      translateBatch: async (templates: string[]) => {
        sent.push(...templates);
        return shout(templates);
      },
    });

    // Translated: prose, headings, the callout title and metadata strings.
    expect(translated).toContain("# GETTING STARTED");
    expect(translated).toContain('title="WHY THIS MATTERS"');
    expect(translated).toContain('title: "GETTING STARTED"');
    expect(translated).toContain("COMPLIANCE IS ENFORCED **IN THE CONTRACT**");

    // Preserved verbatim.
    expect(translated).toContain("pub fn transfer(env: Env) {}");
    expect(translated).toContain("// Comments in code are never translated.");
    expect(translated).toContain('import { Foo } from "@/components/Foo";');
    expect(translated).toContain('canonical: "/docs/getting-started"');
    expect(translated).toContain("(/docs/compliance-guide)");
    expect(translated).toContain("https://stellar.org");
    expect(translated).toContain("`transfer()`");
    expect(translated).toContain("{props.amount}");
    expect(translated).toContain("&amp;");
    expect(translated).toContain('<CalloutBox variant="compliance"');
    expect(translated).toContain("{/* An MDX comment that must survive. */}");
    expect(translated).toContain('<ApiEndpoint method="GET" path="/v1/assets" />');
    expect(translated).toContain("| --- | --- |");
    // Glossary terms are not even sent for translation.
    expect(translated).toContain("**Tessera**");
    expect(sent.join("\n")).not.toMatch(/Tessera|Stellar|transfer\(\)|pub fn|\/docs\//);

    expect(core.validateParity(SOURCE, translated)).toEqual([]);
  });

  it("rejects provider output that drops or duplicates a placeholder", async () => {
    await expect(
      core.translateDocument(SOURCE, {
        locale: "fr",
        retries: 0,
        translateBatch: async (templates: string[]) => templates.map((t) => t.replace(/⟦\d+⟧/g, "")),
      })
    ).rejects.toThrow(/Placeholder mismatch/);

    let attempts = 0;
    const translated = await core.translateDocument("Use `code` here.\n", {
      locale: "fr",
      retries: 1,
      translateBatch: async (templates: string[]) => {
        attempts++;
        return attempts === 1 ? templates.map((t) => `${t} ${t}`) : templates;
      },
    });
    expect(attempts).toBe(2);
    expect(translated).toBe("Use `code` here.\n");
  });

  it("rejects a provider that returns the wrong number of segments", async () => {
    await expect(
      core.translateDocument(SOURCE, { locale: "ja", translateBatch: async () => ["only one"] })
    ).rejects.toThrow(/returned 1 items/);
  });

  it("batches segments by size", async () => {
    const batches: number[] = [];
    await core.translateDocument(SOURCE, {
      locale: "pt",
      maxBatchChars: 40,
      translateBatch: async (templates: string[]) => {
        batches.push(templates.length);
        return templates;
      },
    });
    expect(batches.length).toBeGreaterThan(1);
  });
});

describe("validateParity", () => {
  it("passes for a faithful translation", () => {
    expect(core.validateParity(SOURCE, SOURCE)).toEqual([]);
  });

  it("detects JSX tag drift", () => {
    const broken = SOURCE.replace("</CalloutBox>", "");
    expect(core.validateParity(SOURCE, broken)[0]).toMatch(/JSX tags differ/);
    const renamed = SOURCE.replace("<ApiEndpoint", "<ApiEndPoint");
    expect(core.validateParity(SOURCE, renamed).join()).toMatch(/JSX tags differ.*ApiEndPoint/);
  });

  it("detects edited code blocks, inline code, links and imports", () => {
    expect(core.validateParity(SOURCE, SOURCE.replace("pub fn transfer", "pub fn transferir")).join()).toMatch(/Code blocks differ/);
    expect(core.validateParity(SOURCE, SOURCE.replace("`transfer()`", "`transferir()`")).join()).toMatch(/Inline code spans differ/);
    expect(core.validateParity(SOURCE, SOURCE.replace("(/docs/compliance-guide)", "(/es/docs/guia)")).join()).toMatch(/Link targets differ/);
    expect(core.validateParity(SOURCE, SOURCE.replace("@/components/Foo", "@/components/Bar")).join()).toMatch(/import\/export statements differ/);
    expect(core.validateParity(SOURCE, SOURCE.replace("# Getting", "## Getting")).join()).toMatch(/Heading levels differ/);
    expect(core.validateParity(SOURCE, `${SOURCE}⟦3⟧`).join()).toMatch(/Unrestored placeholder/);
  });

  it("ignores tags that only appear inside code", () => {
    const withCode = "Text\n\n```tsx\n<Widget />\n```\n";
    expect(core.extractJsxTags(withCode)).toEqual([]);
    expect(core.extractJsxTags("Use `<Widget />` inline.")).toEqual([]);
  });
});

describe("provenance and routing", () => {
  it("round-trips the provenance header", () => {
    const sha = core.sourceHash(SOURCE);
    const text = core.withProvenance("# Hola\n", { source: "app/[lang]/docs/x/page.mdx", sha, locale: "es", provider: "pseudo:pseudo" });
    expect(core.readProvenance(text)).toEqual({ source: "app/[lang]/docs/x/page.mdx", sha, locale: "es", provider: "pseudo:pseudo" });
    expect(core.stripProvenance(text)).toBe("# Hola\n");
    expect(core.readProvenance("# Hand-written\n")).toBeNull();
    expect(sha).toMatch(/^[0-9a-f]{16}$/);
  });

  it("generates a locale-switch page that falls back to English", () => {
    const page: string = core.renderLocalePage(["ja", "es"]);
    expect(page.startsWith(core.GENERATED_PAGE_MARKER)).toBe(true);
    expect(page).toContain('import * as en from "./page.en.mdx";');
    expect(page).toContain('import * as es from "./page.es.mdx";');
    expect(page).toContain("const PAGES = { en, es, ja }");
    expect(page).toContain("return PAGES[lang] ?? PAGES.en;");
  });
});

describe("providers", () => {
  it("pseudo-translates deterministically and keeps Markdown markers first", async () => {
    const translate = core.createProvider({ name: "pseudo" });
    expect(await translate(["## Hello ⟦0⟧ world", "- item"], "es")).toEqual(["## [es] Héllö ⟦0⟧ wörld", "- [es] ítém"]);
  });

  it("calls the Anthropic Messages API and parses the JSON array", async () => {
    const fetchImpl = jest.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => ({ content: [{ type: "text", text: 'Here you go:\n["Hola ⟦0⟧"]' }] }),
      text: async () => "",
    }));
    const translate = core.createProvider({ name: "anthropic", model: "claude-sonnet-5-5", apiKey: "k", fetchImpl });

    expect(await translate(["Hello ⟦0⟧"], "es")).toEqual(["Hola ⟦0⟧"]);
    const [url, init] = fetchImpl.mock.calls[0] as unknown as [string, { headers: Record<string, string>; body: string }];
    expect(url).toBe("https://api.anthropic.com/v1/messages");
    expect(init.headers["x-api-key"]).toBe("k");
    const body = JSON.parse(init.body);
    expect(body.model).toBe("claude-sonnet-5-5");
    expect(body.system).toContain("Spanish (es)");
    expect(body.system).toContain("Soroban");
    expect(JSON.parse(body.messages[0].content)).toEqual(["Hello ⟦0⟧"]);
  });

  it("calls an OpenAI-compatible endpoint", async () => {
    const fetchImpl = jest.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => ({ choices: [{ message: { content: '["Bonjour"]' } }] }),
      text: async () => "",
    }));
    const translate = core.createProvider({ name: "openai", model: "gpt-4o", apiKey: "k", baseUrl: "https://llm.internal/v1/", fetchImpl });

    expect(await translate(["Hello"], "fr")).toEqual(["Bonjour"]);
    expect((fetchImpl.mock.calls[0] as unknown as [string])[0]).toBe("https://llm.internal/v1/chat/completions");
  });

  it("surfaces HTTP errors and malformed responses", async () => {
    const failing = core.createProvider({
      name: "anthropic",
      model: "m",
      apiKey: "k",
      fetchImpl: async () => ({ ok: false, status: 401, json: async () => ({}), text: async () => "invalid x-api-key" }),
    });
    await expect(failing(["x"], "es")).rejects.toThrow(/HTTP 401 invalid x-api-key/);
    expect(() => core.parseJsonArray("no array here")).toThrow(/JSON array/);
    expect(() => core.parseJsonArray("[1, 2]")).toThrow(/array of strings/);
    expect(() => core.createProvider({ name: "anthropic", model: "m" })).toThrow(/API key/);
    expect(() => core.createProvider({ name: "other", apiKey: "k" })).toThrow(/Unknown provider/);
  });
});

describe("existing documentation", () => {
  const routesRoot = join(__dirname, "..", "app", "[lang]", "docs");
  const pages: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir)) {
      const full = join(dir, entry);
      if (statSync(full).isDirectory()) walk(full);
      else if (entry === "page.mdx" || entry === "page.en.mdx") pages.push(full);
    }
  };
  walk(routesRoot);

  it.each(pages.map((p) => [p.slice(routesRoot.length + 1), p]))(
    "%s survives a pseudo-translation with full parity",
    async (_name, file) => {
      const source = readFileSync(file, "utf8");
      const translated = await core.translateDocument(source, { locale: "ja", translateBatch: core.createProvider({ name: "pseudo" }) });
      expect(core.validateParity(source, translated)).toEqual([]);
    }
  );
});
