#!/usr/bin/env node
/**
 * CI Gate: Contract Storage Migration & Upgrade Test Verification (Issue #449 / #66)
 *
 * Enforces that any Pull Request modifying Soroban `storage()` calls within `contracts/`
 * must include corresponding upgrade verification tests.
 */

"use strict";

const { execSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const ROOT = path.resolve(__dirname, "..");

function runGit(cmd) {
  try {
    return execSync(cmd, { cwd: ROOT, encoding: "utf-8" }).trim();
  } catch (err) {
    return "";
  }
}

function checkUpgradeTests() {
  console.log("=== Checking Contract Upgrade & Storage Verification CI Gate ===");

  // Detect base branch for comparison
  const baseRef = process.env.GITHUB_BASE_REF || "origin/main";
  let diffFiles = runGit(`git diff --name-only ${baseRef}...HEAD`);
  if (!diffFiles) {
    diffFiles = runGit("git status --porcelain");
  }

  const changedFiles = diffFiles
    .split("\n")
    .map((f) => f.trim())
    .filter(Boolean);

  let contractsWithStorageChanges = [];
  let hasUpgradeTests = false;

  for (const file of changedFiles) {
    const cleanFile = file.replace(/^[MADRCU?!\s]+/, "").trim();
    if (cleanFile.includes("upgrade_test") || cleanFile.includes("storage_schema")) {
      hasUpgradeTests = true;
    }

    if (cleanFile.startsWith("contracts/") && cleanFile.endsWith(".rs")) {
      const fullPath = path.join(ROOT, cleanFile);
      if (fs.existsSync(fullPath)) {
        const content = fs.readFileSync(fullPath, "utf-8");
        if (content.includes("storage().") || content.includes(".storage()")) {
          contractsWithStorageChanges.push(cleanFile);
        }
      }
    }
  }

  console.log(`Found ${contractsWithStorageChanges.length} contract files with storage interaction.`);
  console.log(`Upgrade verification harness detected: ${hasUpgradeTests ? "YES" : "NO"}`);

  if (contractsWithStorageChanges.length > 0 && !hasUpgradeTests) {
    console.error(
      "\n❌ CI Gate Failure: Contract storage modifications detected without an accompanying upgrade test."
    );
    console.error(
      "Please implement a storage migration test using `contracts/common/src/upgrade_test.rs`."
    );
    process.exit(1);
  }

  console.log("✓ CI Gate Passed: Storage upgrade verification requirements satisfied.\n");
  process.exit(0);
}

if (require.main === module) {
  checkUpgradeTests();
}

module.exports = { checkUpgradeTests };
