/**
 * Animation Stability Utility for Playwright Visual Testing (Issue #446 / #63)
 *
 * Implements `waitForStableAnimation`: waits until no CSS animation frames,
 * transitions, or layout mutations fire for a continuous quiet period (default 500ms).
 */

import { Page } from "@playwright/test";

/**
 * Waits until no CSS animation frames or layout changes occur on the page for `quietPeriodMs`.
 *
 * @param page The Playwright Page instance
 * @param quietPeriodMs Milliseconds of continuous stillness required (default: 500ms)
 * @param timeoutMs Maximum total timeout before resolving (default: 5000ms)
 */
export async function waitForStableAnimation(
  page: Page,
  quietPeriodMs: number = 500,
  timeoutMs: number = 5000
): Promise<void> {
  await page.evaluate(
    ({ quietPeriod, maxTimeout }) => {
      return new Promise<void>((resolve) => {
        let lastFrameTime = performance.now();
        let animationFrameId: number;
        const startTime = performance.now();

        // Listen for active CSS transitions & animations
        let activeTransitions = 0;
        const onStart = () => {
          activeTransitions++;
          lastFrameTime = performance.now();
        };
        const onEnd = () => {
          activeTransitions = Math.max(0, activeTransitions - 1);
          lastFrameTime = performance.now();
        };

        window.addEventListener("animationstart", onStart, { passive: true });
        window.addEventListener("animationiteration", onStart, { passive: true });
        window.addEventListener("animationend", onEnd, { passive: true });
        window.addEventListener("transitionstart", onStart, { passive: true });
        window.addEventListener("transitionend", onEnd, { passive: true });

        // MutationObserver to track active DOM shifts
        const observer = new MutationObserver(() => {
          lastFrameTime = performance.now();
        });
        observer.observe(document.body, {
          attributes: true,
          childList: true,
          subtree: true,
        });

        const checkStillness = (now: number) => {
          // If overall timeout exceeded, resolve to avoid hanging
          if (now - startTime >= maxTimeout) {
            cleanup();
            resolve();
            return;
          }

          // If no transitions are active and quiet period has passed since last event/frame
          if (activeTransitions === 0 && now - lastFrameTime >= quietPeriod) {
            cleanup();
            resolve();
            return;
          }

          // Otherwise keep polling on next animation frame
          animationFrameId = requestAnimationFrame(checkStillness);
        };

        const cleanup = () => {
          cancelAnimationFrame(animationFrameId);
          observer.disconnect();
          window.removeEventListener("animationstart", onStart);
          window.removeEventListener("animationiteration", onStart);
          window.removeEventListener("animationend", onEnd);
          window.removeEventListener("transitionstart", onStart);
          window.removeEventListener("transitionend", onEnd);
        };

        animationFrameId = requestAnimationFrame(checkStillness);
      });
    },
    { quietPeriod: quietPeriodMs, maxTimeout: timeoutMs }
  );
}
