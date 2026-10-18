/**
 * Automated Visual Regression Test Suite for Documentation Routes & Components (Issue #244)
 *
 * Captures full-page and component snapshots across desktop and mobile viewports.
 * Enforces pixel-level visual consistency with a strict 0.1% max difference threshold.
 */

import { test, expect } from "@playwright/test";

// Complete catalog of documentation pages to visually benchmark
const DOC_ROUTES = [
  { name: "landing-page", path: "/" },
  { name: "getting-started", path: "/en/docs/getting-started" },
  { name: "architecture", path: "/en/docs/architecture" },
  { name: "compliance-guide", path: "/en/docs/compliance-guide" },
  { name: "web-app", path: "/en/docs/web-app" },
  { name: "contracts-asset-token", path: "/en/docs/contracts-asset-token" },
  { name: "contracts-registry", path: "/en/docs/contracts-registry" },
  { name: "contracts-compliance", path: "/en/docs/contracts-compliance" },
  { name: "contracts-dividend", path: "/en/docs/contracts-dividend" },
  { name: "api-overview", path: "/en/docs/api-overview" },
  { name: "api-assets", path: "/en/docs/api-assets" },
  { name: "api-compliance", path: "/en/docs/api-compliance" },
  { name: "api-dividends", path: "/en/docs/api-dividends" },
  { name: "api-events", path: "/en/docs/api-events" },
  { name: "api-holders", path: "/en/docs/api-holders" },
];

test.describe("Documentation Visual Regression Suite", () => {
  for (const route of DOC_ROUTES) {
    test(`Visual snapshot: ${route.name} (${route.path})`, async ({ page }) => {
      await page.goto(route.path, { waitUntil: "networkidle" });

      // Ensure stable rendering: scroll down to trigger lazy components, then scroll back up
      await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight / 2));
      await page.waitForTimeout(150);
      await page.evaluate(() => window.scrollTo(0, 0));
      await page.waitForTimeout(150);

      // Capture full-page screenshot and verify against baseline with strict 0.1% diff limit
      await expect(page).toHaveScreenshot(`${route.name}.png`, {
        fullPage: true,
        maxDiffPixelRatio: 0.001, // 0.1% max visual difference
        animations: "disabled",
      });
    });
  }
});

test.describe("UI Component Isolation Visual Snapshots", () => {
  test("Visual snapshot: Header Navigation & Brand Element", async ({ page }) => {
    await page.goto("/en/docs/getting-started", { waitUntil: "networkidle" });
    const header = page.locator("header");
    await expect(header).toBeVisible();

    await expect(header).toHaveScreenshot("component-header.png", {
      maxDiffPixelRatio: 0.001,
      animations: "disabled",
    });
  });

  test("Visual snapshot: Sidebar Navigation Component", async ({ page, isMobile }) => {
    test.skip(isMobile, "Desktop sidebar is hidden on mobile viewports");

    await page.goto("/en/docs/getting-started", { waitUntil: "networkidle" });
    const sidebar = page.locator("aside");
    if ((await sidebar.count()) > 0) {
      await expect(sidebar).toBeVisible();
      await expect(sidebar).toHaveScreenshot("component-sidebar.png", {
        maxDiffPixelRatio: 0.001,
        animations: "disabled",
      });
    }
  });

  test("Visual snapshot: Push Notification Modal", async ({ page }) => {
    await page.goto("/", { waitUntil: "networkidle" });
    const bellBtn = page.locator('button[aria-label="Open push notification settings"]');
    if ((await bellBtn.count()) > 0) {
      await bellBtn.click();
      const modal = page.locator('div[role="dialog"][aria-labelledby="notification-modal-title"]');
      await expect(modal).toBeVisible();
      await expect(modal).toHaveScreenshot("component-notification-modal.png", {
        maxDiffPixelRatio: 0.001,
        animations: "disabled",
      });
    }
  });
});
