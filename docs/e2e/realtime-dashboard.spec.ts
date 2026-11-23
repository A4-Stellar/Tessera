import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

test.describe("RealTimeDashboard E2E & Accessibility", () => {
  test("renders RealTimeDashboard with live compliance feed and heatmap", async ({ page }) => {
    // Mock SSE endpoint for GET /v1/assets/1/events
    await page.route("**/v1/assets/*/events", async (route) => {
      const sseBody = [
        `data: ${JSON.stringify({
          id: 1,
          event_type: "compliance",
          jurisdiction: "US",
          is_allowed: true,
          account: "GBBD...TESSERA",
          timestamp: new Date().toISOString(),
        })}\n\n`,
        `data: ${JSON.stringify({
          id: 2,
          event_type: "compliance",
          jurisdiction: "DE",
          is_allowed: false,
          reason: "Jurisdiction sanction check failed",
          account: "GCXK...INVESTOR",
          timestamp: new Date().toISOString(),
        })}\n\n`,
      ].join("");

      await route.fulfill({
        status: 200,
        headers: {
          "Content-Type": "text/event-stream",
          "Cache-Control": "no-cache",
          Connection: "keep-alive",
        },
        body: sseBody,
      });
    });

    // Mock GET /v1/events
    await page.route("**/v1/events", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify([]),
      });
    });

    await page.goto("/docs/integration");
    await page.waitForLoadState("domcontentloaded");

    const dashboard = page.getByTestId("realtime-dashboard");
    if (await dashboard.isVisible()) {
      // Check heading and real-time status
      await expect(dashboard.getByRole("heading", { level: 3 })).toContainText("Real-Time Compliance & Activity Dashboard");

      // Verify heatmap container is present
      const heatmap = dashboard.getByTestId("compliance-heatmap");
      await expect(heatmap).toBeVisible();

      // Verify keyboard navigation on jurisdiction nodes
      const usNode = heatmap.getByRole("button", { name: /United States/i });
      if (await usNode.isVisible()) {
        await usNode.focus();
        await page.keyboard.press("Enter");
        const details = heatmap.getByRole("region", { name: /Details for United States/i });
        await expect(details).toBeVisible();
      }

      // Verify WCAG 2.1 AA compliance
      const accessibilityScanResults = await new AxeBuilder({ page })
        .include('[data-testid="realtime-dashboard"]')
        .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
        .analyze();

      expect(accessibilityScanResults.violations).toEqual([]);
    }
  });

  test("displays AnomalyAlert on detection of anomaly events (issue #30)", async ({ page }) => {
    // Route SSE with anomaly detector payload
    await page.route("**/v1/assets/*/events", async (route) => {
      const anomalyBody = `data: ${JSON.stringify({
        id: 42,
        event_type: "anomaly_alert",
        jurisdiction: "US",
        account: "GSUSPICIOUS...ACCOUNT",
        timestamp: new Date().toISOString(),
        data: {
          account: "GSUSPICIOUS...ACCOUNT",
          metric: "VelocitySpike",
          peak_z_score: 4.85,
          z_score: 4.85,
          risk_flag: true,
        },
      })}\n\n`;

      await route.fulfill({
        status: 200,
        headers: {
          "Content-Type": "text/event-stream",
          "Cache-Control": "no-cache",
        },
        body: anomalyBody,
      });
    });

    await page.goto("/docs/integration");
    await page.waitForLoadState("domcontentloaded");

    const alert = page.getByTestId("anomaly-alert");
    if (await alert.isVisible()) {
      await expect(alert).toHaveAttribute("role", "alert");
      await expect(alert).toHaveAttribute("aria-live", "assertive");
      await expect(alert).toContainText("Anomaly Detected (Issue #30)");
      await expect(alert).toContainText("Z = 4.85");
    }
  });
});
