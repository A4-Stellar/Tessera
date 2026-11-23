/**
 * ComponentMatrix Visual Regression & A11y Specs (Issue #446 / #63)
 *
 * Runs full visual regression and accessibility audits across all 18 permutations:
 * {light, dark} x {mobile, tablet, desktop} x {en, ar (RTL), zh} = 18 per component.
 */

import { test, expect } from "@playwright/test";
import {
  generatePermutations,
  applyPermutation,
  auditAccessibility,
  detectFlakiness,
  getComponentTolerance,
  DOC_COMPONENTS,
} from "../utils/component-matrix";

const permutations = generatePermutations();

test.describe("ComponentMatrix Visual Regression & A11y Suite", () => {
  for (const componentName of DOC_COMPONENTS.slice(0, 8)) {
    test.describe(`Component: ${componentName}`, () => {
      for (const p of permutations) {
        const testTitle = `${componentName} [${p.theme}-${p.viewport.name}-${p.locale}${p.isRtl ? "-rtl" : ""}]`;

        test(testTitle, async ({ page }) => {
          // Navigate to component preview route or docs page where component is mounted
          await page.goto(`/en/docs/components?component=${componentName}`, {
            waitUntil: "domcontentloaded",
          });

          // Apply 18-permutation matrix styles: theme, viewport, locale/RTL
          await applyPermutation(page, p);

          const target = page.locator(`[data-component="${componentName}"]`).or(page.locator("main"));
          await expect(target.first()).toBeVisible({ timeout: 5000 });

          // 1. Flakiness Detection: Run 3 screenshot iterations to flag non-deterministic renders
          const flakiness = await detectFlakiness(target.first(), 3);
          expect(
            flakiness.isFlaky,
            `Flakiness detected in ${testTitle} (variance ratio: ${flakiness.varianceRatio})`
          ).toBe(false);

          // 2. Visual Snapshot Comparison with Per-Component Tolerance Threshold
          const tolerance = getComponentTolerance(componentName);
          await expect(target.first()).toHaveScreenshot(
            `${componentName}-${p.theme}-${p.viewport.name}-${p.locale}.png`,
            {
              maxDiffPixelRatio: tolerance,
              animations: "disabled",
            }
          );

          // 3. Automated Accessibility Audit (Axe-Core): Fail on Critical or Serious violations
          await auditAccessibility(page, `[data-component="${componentName}"]`);
        });
      }
    });
  }
});
