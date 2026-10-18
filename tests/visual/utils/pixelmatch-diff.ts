/**
 * Pixelmatch Visual Regression Utility (Issue #244)
 *
 * Provides standalone pixel-diff verification using the pixelmatch algorithm:
 * - Compares baseline PNG image buffer against candidate PR screenshot buffer
 * - Computes total mismatched pixels and exact diff percentage (mismatched / total)
 * - Asserts diff percentage does not exceed 0.1% (0.001)
 * - Renders and writes visual differential artifact PNG when threshold is violated
 *
 * Complexity:
 * - Time: O(W * H) where W = width, H = height in pixels.
 * - Space: O(W * H) for RGBA diff pixel buffer.
 */

import fs from "fs";
import path from "path";
import pixelmatch from "pixelmatch";
import { PNG } from "pngjs";

export interface PixelDiffOptions {
  threshold?: number; // Matching sensitivity (0.0 to 1.0). Default 0.1
  maxDiffPixelRatio?: number; // Maximum allowed pixel diff ratio. Default 0.001 (0.1%)
  includeAA?: boolean; // Whether to include anti-aliased pixels. Default false
  diffOutputPath?: string; // Path to write visual diff image if threshold exceeded
}

export interface PixelDiffResult {
  passed: boolean;
  mismatchedPixels: number;
  totalPixels: number;
  diffPercentage: number;
  diffImagePath?: string;
}

/**
 * Compares two PNG buffers pixel-by-pixel using pixelmatch.
 * @param {Buffer} baselineBuffer
 * @param {Buffer} candidateBuffer
 * @param {PixelDiffOptions} options
 * @returns {PixelDiffResult}
 */
export function compareImages(
  baselineBuffer: Buffer,
  candidateBuffer: Buffer,
  options: PixelDiffOptions = {}
): PixelDiffResult {
  const threshold = options.threshold ?? 0.1;
  const maxDiffPixelRatio = options.maxDiffPixelRatio ?? 0.001; // 0.1% strict threshold
  const includeAA = options.includeAA ?? false;

  const img1 = PNG.sync.read(baselineBuffer);
  const img2 = PNG.sync.read(candidateBuffer);

  // Dimension validation
  if (img1.width !== img2.width || img1.height !== img2.height) {
    throw new Error(
      `Image dimension mismatch: baseline is ${img1.width}x${img1.height}, candidate is ${img2.width}x${img2.height}`
    );
  }

  const { width, height } = img1;
  const totalPixels = width * height;
  const diff = new PNG({ width, height });

  // Run pixelmatch diffing algorithm
  const mismatchedPixels = pixelmatch(
    img1.data,
    img2.data,
    diff.data,
    width,
    height,
    {
      threshold,
      includeAA,
      diffColor: [255, 0, 0], // Bright Red for differences
      diffColorAlt: [0, 255, 0], // Green for alternate differences
    }
  );

  const diffPercentage = (mismatchedPixels / totalPixels) * 100;
  const passed = diffPercentage <= maxDiffPixelRatio * 100;

  let diffImagePath: string | undefined;

  // If failed and output path provided, write diff image artifact
  if (!passed && options.diffOutputPath) {
    const outputDir = path.dirname(options.diffOutputPath);
    if (!fs.existsSync(outputDir)) {
      fs.mkdirSync(outputDir, { recursive: true });
    }
    fs.writeFileSync(options.diffOutputPath, PNG.sync.write(diff));
    diffImagePath = options.diffOutputPath;
  }

  return {
    passed,
    mismatchedPixels,
    totalPixels,
    diffPercentage,
    diffImagePath,
  };
}

/**
 * Asserts that two screenshot buffers have less than 0.1% visual difference.
 */
export function assertVisualSnapshot(
  baselineBuffer: Buffer,
  candidateBuffer: Buffer,
  testName: string,
  outputDir: string = "./test-results/diffs"
): void {
  const diffPath = path.join(outputDir, `${testName.replace(/[^a-z0-9]/gi, "_")}-diff.png`);
  const result = compareImages(baselineBuffer, candidateBuffer, {
    maxDiffPixelRatio: 0.001, // 0.1%
    threshold: 0.1,
    diffOutputPath: diffPath,
  });

  if (!result.passed) {
    throw new Error(
      `Visual regression detected in "${testName}": ${result.mismatchedPixels} pixels mismatched ` +
        `(${result.diffPercentage.toFixed(4)}% > allowed 0.1%). Diff image saved to ${diffPath}`
    );
  }
}
