/* eslint-disable @typescript-eslint/no-require-imports */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { getLocaleDirection, isRtlLocale, LOCALES } from "@/lib/i18n";

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

  it("freezes {/* i18n-freeze */} blocks and automatically protects Stellar contract and account addresses", async () => {
    const contractAddr = "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ";
    const accountAddr = "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA";
    const frozenSource = `# Deployment

Deploy to contract ${contractAddr} owned by ${accountAddr}.

{/* i18n-freeze */}
| Contract | Address |
| --- | --- |
| asset-token | ${contractAddr} |
{/* /i18n-freeze */}

Inline {/* i18n-freeze */}do not translate this span{/* /i18n-freeze */} end.
`;

    const sent: string[] = [];
    const translated: string = await core.translateDocument(frozenSource, {
      locale: "es",
      translateBatch: async (templates: string[]) => {
        sent.push(...templates);
        return shout(templates);
      },
    });

    expect(translated).toContain("# DEPLOYMENT");
    expect(translated).toContain(`DEPLOY TO CONTRACT ${contractAddr} OWNED BY ${accountAddr}.`);
    expect(translated).toContain("| asset-token | CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ |");
    expect(translated).toContain("{/* i18n-freeze */}do not translate this span{/* /i18n-freeze */}");
    expect(sent.join("\n")).not.toContain(contractAddr);
    expect(sent.join("\n")).not.toContain(accountAddr);
    expect(sent.join("\n")).not.toContain("do not translate this span");
    expect(core.validateParity(frozenSource, translated)).toEqual([]);

    const tamperedAddr = translated.replace(
      contractAddr,
      "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4"
    );
    expect(core.validateParity(frozenSource, tamperedAddr).join("\n")).toMatch(/Contract addresses differ/);
  });

  it("caches segments in translation_memory.json keyed by SHA-256 content hash", async () => {
    const tm = core.createTranslationMemory();
    const providerSpy = jest.fn(async (templates: string[]) => shout(templates));

    const first = await core.translateDocument(SOURCE, {
      locale: "fr",
      translateBatch: providerSpy,
      translationMemory: tm,
    });

    expect(providerSpy).toHaveBeenCalledTimes(1);
    const entryKeys = Object.keys(tm.entries);
    expect(entryKeys.length).toBeGreaterThan(0);
    for (const key of entryKeys) {
      expect(key).toMatch(/^[0-9a-f]{64}$/);
      expect(tm.entries[key].translations.fr).toBeDefined();
    }

    // Second run with the same source and locale hits SHA-256 translation memory without calling the API
    providerSpy.mockClear();
    const second = await core.translateDocument(SOURCE, {
      locale: "fr",
      translateBatch: providerSpy,
      translationMemory: tm,
    });
    expect(providerSpy).not.toHaveBeenCalled();
    expect(second).toBe(first);
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

  it("supports ES, FR, ZH, JA and AR (RTL) locales", () => {
    for (const loc of ["en", "es", "fr", "zh", "ja", "ar"]) {
      expect(LOCALES).toContain(loc);
    }
    expect(isRtlLocale("ar")).toBe(true);
    expect(getLocaleDirection("ar")).toBe("rtl");
    expect(isRtlLocale("es")).toBe(false);
    expect(getLocaleDirection("zh")).toBe("ltr");
    expect(core.getLocaleDirection("ar")).toBe("rtl");
    expect(core.getLocaleDirection("ja")).toBe("ltr");
  });
});

describe("providers", () => {
  it("pseudo-translates deterministically and keeps Markdown markers first", async () => {
    const translate = core.createProvider({ name: "pseudo" });
    expect(await translate(["## Hello ⟦0⟧ world", "- item"], "es")).toEqual(["## [es] Héllö ⟦0⟧ wörld", "- [es] ítém"]);
  });

  it("calls the Gemini generateContent API and parses the JSON array", async () => {
    const fetchImpl = jest.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => ({
        candidates: [
          {
            content: {
              parts: [{ text: '["مرحبا ⟦0⟧"]' }],
            },
          },
        ],
      }),
      text: async () => "",
    }));
    const translate = core.createProvider({
      name: "gemini",
      model: "gemini-2.0-flash",
      apiKey: "gemini-test-key",
      fetchImpl,
    });

    expect(await translate(["Hello ⟦0⟧"], "ar")).toEqual(["مرحبا ⟦0⟧"]);
    const [url, init] = fetchImpl.mock.calls[0] as unknown as [string, { headers: Record<string, string>; body: string }];
    expect(url).toBe(
      "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key=gemini-test-key"
    );
    const body = JSON.parse(init.body);
    expect(body.systemInstruction.parts[0].text).toContain("Arabic (ar)");
    expect(JSON.parse(body.contents[0].parts[0].text)).toEqual(["Hello ⟦0⟧"]);
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

describe("consistency checker, glossary validator, quality metrics, and MDX linter", () => {
  it("flags inconsistent translations of the same English term across files for the same locale", () => {
    const docs = [
      {
        route: "getting-started",
        locale: "es",
        source: "# Prerequisites\n\nOnly addresses on an **allowlist** may hold the token.\n",
        translated: "# Requisitos previos\n\nSolo las direcciones en una **lista blanca** pueden poseer el token.\n",
      },
      {
        route: "compliance-guide",
        locale: "es",
        source: "# Prerequisites\n\nOnly addresses on an **allowlist** may hold the token.\n",
        translated: "# Prerrequisitos\n\nSolo las direcciones en una **lista permitida** pueden poseer el token.\n",
      },
    ];

    const issues = core.checkTerminologyConsistency(docs);
    expect(issues.length).toBeGreaterThanOrEqual(2);
    expect(issues.map((i: { englishTerm: string }) => i.englishTerm)).toEqual(
      expect.arrayContaining(["Prerequisites", "allowlist"])
    );
  });

  it("auto-validates translation-glossary.json and fails on unknown technical terms", () => {
    const validResult = core.validateGlossary(
      { terms: ["Merkle", "stroops"] },
      [{ route: "getting-started", source: "The **Tessera** SDK uses Soroban XDR and Merkle trees.\n" }]
    );
    expect(validResult.valid).toBe(true);
    expect(validResult.errors).toEqual([]);

    const invalidResult = core.validateGlossary(
      { terms: ["Merkle"] },
      [{ route: "architecture", source: "The contract verifies ZKSTARK proofs over QUIC.\n" }]
    );
    expect(invalidResult.valid).toBe(false);
    expect(invalidResult.unknownTerms).toEqual(["QUIC", "ZKSTARK"]);
    expect(invalidResult.errors.join("\n")).toContain('Unknown technical term "ZKSTARK"');
  });

  it("computes BLEU-4 score, terminology coverage, and Markdown quality reports", () => {
    const ref = "The Tessera toolkit tokenizes real-world assets on Stellar with Soroban smart contracts.";
    expect(core.computeBleuScore(ref, ref)).toBeCloseTo(1, 3);
    expect(
      core.computeBleuScore(
        ref,
        "The Tessera platform tokenizes real-world assets on Stellar using Soroban contracts."
      )
    ).toBeGreaterThan(0.35);

    const covFull = core.computeTerminologyCoverage(
      "Tessera uses Stellar and Soroban.",
      "Tessera utiliza Stellar y Soroban."
    );
    expect(covFull.coverage).toBe(1);
    expect(covFull.missing).toEqual([]);

    const covPartial = core.computeTerminologyCoverage(
      "Tessera uses Stellar and Soroban.",
      "Tessera utiliza Stellar."
    );
    expect(covPartial.coverage).toBeCloseTo(2 / 3, 3);
    expect(covPartial.missing).toEqual(["Soroban"]);

    const report = core.generateQualityReport([
      {
        route: "getting-started",
        locale: "ar",
        source: ref,
        translated: ref,
      },
    ]);
    expect(report.summary.allPassed).toBe(true);
    expect(report.files[0].direction).toBe("rtl");
    expect(report.markdown).toContain("MDX i18n Translation Quality Report");
    expect(report.markdown).toContain("`getting-started`");
    expect(report.markdown).toContain("`rtl`");
  });

  it("lints translated MDX for broken links and invalid code blocks", () => {
    const cleanSource = "# Guide\n\nSee [Overview](/docs/api/overview).\n\n```json\n{\"ok\": true}\n```\n";
    expect(core.lintTranslatedMdx(cleanSource, cleanSource).valid).toBe(true);

    const brokenJson = "# Guide\n\nSee [Overview](/docs/api/overview).\n\n```json\n{\"ok\": }\n```\n";
    const jsonLint = core.lintTranslatedMdx(cleanSource, brokenJson);
    expect(jsonLint.valid).toBe(false);
    expect(jsonLint.errors.join("\n")).toMatch(/Invalid JSON in code block/);

    const unclosedFence = "# Guide\n\n```ts\nconst x = 1;\n";
    const fenceLint = core.lintTranslatedMdx(cleanSource, unclosedFence);
    expect(fenceLint.valid).toBe(false);
    expect(fenceLint.errors.join("\n")).toMatch(/Unclosed fenced code block/);

    const emptyLink = "# Guide\n\nSee [Overview]().\n";
    const linkLint = core.lintTranslatedMdx(cleanSource, emptyLink);
    expect(linkLint.valid).toBe(false);
    expect(linkLint.errors.join("\n")).toMatch(/Empty link target/);
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
