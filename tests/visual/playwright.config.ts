/**
 * Playwright Automated Visual Regression Testing Configuration (Issue #244)
 *
 * Implements automated visual regression testing across desktop and mobile viewports:
 * - Desktop Chrome (1280x800)
 * - Desktop Firefox (1280x800)
 * - Desktop Safari / WebKit (1280x800)
 * - Mobile Chrome (Pixel 5, 393x851)
 * - Mobile Safari (iPhone 12, 390x844)
 *
 * Visual Differential Invariant:
 * - Fail CI jobs and emit diff images when visual pixel differences exceed 0.1% (maxDiffPixelRatio: 0.001).
 *
 * Time Complexity: O(W * H) per image comparison where W=width, H=height.
 * Space Complexity: O(W * H) raw pixel buffer allocation during diff rendering.
 */

import { defineConfig, devices } from "@playwright/test";
import path from "path";

/**
 * Strict 0.1% maximum allowed pixel difference ratio as required by Issue #244.
 */
export const MAX_DIFF_PIXEL_RATIO = 0.001; // 0.1%

export default defineConfig({
  testDir: "./specs",
  outputDir: "./test-results",
  snapshotDir: "./baselines",
  snapshotPathTemplate: "{testDir}/../baselines/{projectName}/{testFileDir}/{testFileName}/{arg}{ext}",

  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 2 : undefined,

  reporter: [
    ["list"],
    ["html", { outputFolder: "./test-results/html-report", open: "never" }],
    ["json", { outputFile: "./test-results/visual-regression-results.json" }],
  ],

  expect: {
    toHaveScreenshot: {
      maxDiffPixelRatio: MAX_DIFF_PIXEL_RATIO, // 0.1% threshold
      threshold: 0.1, // pixelmatch color sensitivity threshold (0.0 to 1.0)
      animations: "disabled",
      caret: "hide",
      scale: "css",
    },
    toMatchSnapshot: {
      maxDiffPixelRatio: MAX_DIFF_PIXEL_RATIO,
      threshold: 0.1,
    },
  },

  use: {
    baseURL: process.env.PLAYWRIGHT_TEST_BASE_URL || "http://localhost:3000",
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },

  projects: [
    // -------------------------------------------------------------------------
    // Desktop Viewports (1280x800)
    // -------------------------------------------------------------------------
    {
      name: "desktop-chrome",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1280, height: 800 },
      },
    },
    {
      name: "desktop-firefox",
      use: {
        ...devices["Desktop Firefox"],
        viewport: { width: 1280, height: 800 },
      },
    },
    {
      name: "desktop-safari",
      use: {
        ...devices["Desktop Safari"],
        viewport: { width: 1280, height: 800 },
      },
    },

    // -------------------------------------------------------------------------
    // Mobile Viewports
    // -------------------------------------------------------------------------
    {
      name: "mobile-chrome-pixel5",
      use: {
        ...devices["Pixel 5"],
      },
    },
    {
      name: "mobile-safari-iphone12",
      use: {
        ...devices["iPhone 12"],
      },
    },
  ],

  webServer: {
    command: "npm run start",
    cwd: path.resolve(__dirname, "../../docs"),
    url: "http://localhost:3000",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
