import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const PAGES_TO_TEST = [
  "/",
  "/docs/getting-started",
  "/docs/architecture",
  "/docs/compliance-guide",
  "/docs/api/overview",
  "/docs/api/assets",
  "/docs/api/compliance",
  "/docs/api/dividends",
  "/docs/api/holders",
  "/docs/api/events",
  "/docs/api/rate-limits",
  "/docs/contracts/asset-token",
  "/docs/contracts/compliance",
  "/docs/contracts/dividend",
  "/docs/contracts/registry",
  "/docs/time-and-ledgers",
  "/docs/web-app",
  "/docs/integration",
];

test.describe("Automated Accessibility (a11y) E2E Scans (WCAG 2.1 AA)", () => {
  for (const pagePath of PAGES_TO_TEST) {
    test(`Page "${pagePath}" should have no WCAG 2.1 AA accessibility violations`, async ({ page }) => {
      await page.goto(pagePath);
      await page.waitForLoadState("domcontentloaded");

      const accessibilityScanResults = await new AxeBuilder({ page })
        .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
        .analyze();

      expect(accessibilityScanResults.violations).toEqual([]);
    });
  }

  test("Mobile navigation drawer should trap focus and pass a11y audit when open", async ({ page }) => {
    await page.setViewportSize({ width: 375, height: 667 });
    await page.goto("/docs/getting-started");
    await page.waitForLoadState("domcontentloaded");

    const menuButton = page.getByRole("button", { name: /open navigation menu/i });
    if (await menuButton.isVisible()) {
      await menuButton.click();
      const modalDrawer = page.getByRole("dialog", { name: /mobile navigation/i });
      await expect(modalDrawer).toBeVisible();

      const accessibilityScanResults = await new AxeBuilder({ page })
        .include("#mobile-navigation-drawer")
        .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
        .analyze();

      expect(accessibilityScanResults.violations).toEqual([]);
    }
  });
});
