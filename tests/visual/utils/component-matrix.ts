/**
 * ComponentMatrix Test Runner & A11y Auditor (Issue #446 / #63)
 *
 * Implements:
 * 1. 18-permutation matrix per component: {light, dark} x {mobile, tablet, desktop} x {en, ar (RTL), zh}
 * 2. Per-component pixel tolerance thresholds (0.1% strict, 0.5% for WebGL/Canvas visualizers)
 * 3. Automated accessibility audits using axe-core (@axe-core/playwright) failing on serious/critical violations
 * 4. Flakiness detection running each spec 3 times to flag non-deterministic snapshots
 */

import { Page, Locator, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { waitForStableAnimation } from "./animation";

export interface ViewportSize {
  width: number;
  height: number;
  name: "mobile" | "tablet" | "desktop";
}

export type Theme = "light" | "dark";
export type Locale = "en" | "ar" | "zh";

export interface MatrixPermutation {
  theme: Theme;
  viewport: ViewportSize;
  locale: Locale;
  isRtl: boolean;
}

export const VIEWPORTS: Record<"mobile" | "tablet" | "desktop", ViewportSize> = {
  mobile: { width: 375, height: 667, name: "mobile" },
  tablet: { width: 768, height: 1024, name: "tablet" },
  desktop: { width: 1280, height: 800, name: "desktop" },
};

export const THEMES: Theme[] = ["light", "dark"];
export const LOCALES: { code: Locale; isRtl: boolean }[] = [
  { code: "en", isRtl: false },
  { code: "ar", isRtl: true },
  { code: "zh", isRtl: false },
];

/**
 * 25 Core Documentation Components with dedicated visual verification
 */
export const DOC_COMPONENTS = [
  "AnomalyAlert",
  "ApiEndpoint",
  "ApiPlayground",
  "AuditDashboard",
  "BridgeAssistant",
  "CalloutBox",
  "CapTableCompare",
  "CapTableHeatmap",
  "CapTableTimeline",
  "CapTableVisualizer",
  "CodeBlock",
  "ComplianceHeatmap",
  "ComplianceMatrix",
  "ContractMetadataPanel",
  "DividendSimulator",
  "DocHeader",
  "ErrorCodeTable",
  "EventExplorer",
  "ExemptionChecker",
  "GasVisualizer",
  "GovernancePortal",
  "GraphQLQueryBuilder",
  "IssuerWizard",
  "NetworkGraph",
  "NotificationModal",
  "ProspectusViewer",
  "RealTimeDashboard",
  "RegulatoryMatrix",
  "ResourceBudgetVisualizer",
  "SearchModal",
  "Sidebar",
  "SorobanIDE",
  "WasmSandbox",
  "WaterfallVisualizer",
] as const;

export type DocComponent = typeof DOC_COMPONENTS[number];

/**
 * Per-component tolerance mapping:
 * - Text/tabular components require strict 0.001 (0.1%)
 * - Canvas/SVG visualizers (heatmaps, network graphs, IDEs) allow 0.005 (0.5%) for font-smoothing/GPU jitter
 */
export const COMPONENT_TOLERANCES: Record<string, number> = {
  NetworkGraph: 0.005,
  CapTableHeatmap: 0.005,
  ComplianceHeatmap: 0.005,
  WaterfallVisualizer: 0.005,
  GasVisualizer: 0.005,
  SorobanIDE: 0.005,
  WasmSandbox: 0.005,
  RealTimeDashboard: 0.003,
  AuditDashboard: 0.003,
  DocumentSigningCanvas: 0.005,
  default: 0.001, // 0.1% default strict threshold
};

export function getComponentTolerance(componentName: string): number {
  return COMPONENT_TOLERANCES[componentName] ?? COMPONENT_TOLERANCES.default;
}

/**
 * Generates all 18 permutations: 2 themes x 3 viewports x 3 locales = 18
 */
export function generatePermutations(): MatrixPermutation[] {
  const permutations: MatrixPermutation[] = [];

  for (const theme of THEMES) {
    for (const vpKey of ["mobile", "tablet", "desktop"] as const) {
      const viewport = VIEWPORTS[vpKey];
      for (const loc of LOCALES) {
        permutations.push({
          theme,
          viewport,
          locale: loc.code,
          isRtl: loc.isRtl,
        });
      }
    }
  }

  return permutations;
}

export interface FlakinessResult {
  isFlaky: boolean;
  varianceRatio: number;
}

/**
 * Applies matrix permutation styles to the page.
 */
export async function applyPermutation(page: Page, p: MatrixPermutation): Promise<void> {
  // Set viewport
  await page.setViewportSize({ width: p.viewport.width, height: p.viewport.height });

  // Apply theme & RTL direction
  await page.evaluate(
    ({ theme, isRtl, locale }) => {
      document.documentElement.classList.remove("light", "dark");
      document.documentElement.classList.add(theme);
      document.documentElement.setAttribute("data-theme", theme);
      document.documentElement.setAttribute("dir", isRtl ? "rtl" : "ltr");
      document.documentElement.setAttribute("lang", locale);
    },
    { theme: p.theme, isRtl: p.isRtl, locale: p.locale }
  );

  // Wait for animations and CSS reflow to stabilize
  await waitForStableAnimation(page, 500);
}

/**
 * Runs automated accessibility audit using axe-core, failing if critical or serious violations exist.
 */
export async function auditAccessibility(page: Page, context?: string | Locator): Promise<void> {
  let builder = new AxeBuilder({ page });
  if (typeof context === "string") {
    builder = builder.include(context);
  }

  const results = await builder
    .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
    .analyze();

  const seriousOrCritical = results.violations.filter(
    (v) => v.impact === "critical" || v.impact === "serious"
  );

  if (seriousOrCritical.length > 0) {
    const formatted = seriousOrCritical
      .map((v) => `[${v.impact?.toUpperCase()}] ${v.id}: ${v.description} (${v.nodes.length} occurrences)`)
      .join("\n");
    throw new Error(`Accessibility audit failed with ${seriousOrCritical.length} critical/serious violations:\n${formatted}`);
  }
}

/**
 * Detects snapshot flakiness by capturing the component 3 times in succession
 * and ensuring buffer variance does not exceed the allowed threshold.
 */
export async function detectFlakiness(
  target: Page | Locator,
  runs: number = 3
): Promise<FlakinessResult> {
  const buffers: Buffer[] = [];

  for (let i = 0; i < runs; i++) {
    const buf = await target.screenshot({ animations: "disabled" });
    buffers.push(buf);
  }

  // Compare buffers 0-1, 1-2, 0-2
  let maxMismatchBytes = 0;
  for (let i = 0; i < buffers.length - 1; i++) {
    const a = buffers[i];
    const b = buffers[i + 1];
    if (a.length !== b.length) {
      maxMismatchBytes = Math.max(maxMismatchBytes, Math.abs(a.length - b.length));
    } else {
      let diff = 0;
      for (let j = 0; j < a.length; j++) {
        if (a[j] !== b[j]) diff++;
      }
      maxMismatchBytes = Math.max(maxMismatchBytes, diff);
    }
  }

  const totalBytes = buffers[0].length || 1;
  const varianceRatio = maxMismatchBytes / totalBytes;

  return {
    isFlaky: varianceRatio > 0.0005, // > 0.05% non-deterministic variation
    varianceRatio,
  };
}
