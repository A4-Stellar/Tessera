import { readFileSync, readdirSync, statSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { remark } from 'remark';
import { visit } from 'unist-util-visit';
import { buildAllAbiArtifacts, syncGeneratedFiles } from './lib/abi-codegen.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '../..');
const docsDir = path.resolve(__dirname, '../content/en/docs');
const sdkTestsDir = path.resolve(repoRoot, 'sdk/tests/generated');
const CHECK_ONLY = process.argv.includes('--check');

function getMdxFiles(dir) {
  let results = [];
  if (!existsSync(dir)) return results;
  const list = readdirSync(dir).sort();
  for (const file of list) {
    const filePath = path.join(dir, file);
    const stat = statSync(filePath);
    if (stat && stat.isDirectory()) {
      if (file !== 'node_modules' && file !== '.next') {
        results = results.concat(getMdxFiles(filePath));
      }
    } else if (filePath.endsWith('.mdx') || filePath.endsWith('.md')) {
      results.push(filePath);
    }
  }
  return results;
}

function extractCodeBlocks(content) {
  const blocks = [];
  try {
    const tree = remark().parse(content);
    visit(tree, 'code', (node) => {
      const lang = node.lang ? node.lang.toLowerCase() : null;
      if (['typescript', 'ts', 'tsx', 'python', 'go'].includes(lang)) {
        blocks.push({ lang, value: node.value, loc: node.position });
      }
    });
  } catch {
    // Skip unparseable files
  }
  return blocks;
}

function sanitizeName(name) {
  return name.replace(/[^a-zA-Z0-9_-]/g, '_').replace(/_+/g, '_').replace(/^_|_$/g, '');
}

function generateTypeScriptTestFile(blocks, sourceName) {
  const importStatements = `import { TesseraClient } from '@tessera/sdk';\nimport { describe, it, expect } from 'vitest';\n\nconst API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";\nconst client = new TesseraClient(API);\n\n`;

  const testCases = blocks
    .map((block, i) => {
      const code = block.value;
      return `  it('executes TypeScript example from ${sourceName} block ${i + 1}', async () => {\n    // Source: ${block.source}:${block.loc.start.line}\n${code
        .split('\n')
        .map((l) => `    ${l}`)
        .join('\n')}\n  });`;
    })
    .join('\n\n');

  return `${importStatements}describe('SDK TypeScript Examples - ${sourceName}', () => {\n${testCases}\n});\n`;
}

function generatePythonTestFile(blocks, sourceName) {
  const header = `import pytest\nfrom tessera_sdk import TesseraClient\n\nAPI_BASE = "http://localhost:8080"\nclient = TesseraClient(API_BASE)\n\n`;

  const testCases = blocks
    .map((block, i) => {
      const code = block.value;
      const indented = code
        .split('\n')
        .map((l) => `    ${l}`)
        .join('\n');
      return `  async def test_python_example_${i + 1}_${sanitizeName(sourceName)}(self):\n    # Source: ${block.source}:${block.loc.start.line}\n${indented}\n`;
    })
    .join('\n');

  return `${header}class TestSDKPythonExamples:\n${testCases}\n`;
}

function generateGoTestFile(blocks, sourceName) {
  const header = `package generated_test\n\nimport (\n\t"testing"\n\t"github.com/stretchr/testify/assert"\n)\n\nconst apiBase = "http://localhost:8080"\n\n`;

  const testCases = blocks
    .map((block, i) => {
      const code = block.value;
      const indented = code
        .split('\n')
        .map((l) => `\t${l}`)
        .join('\n');
      return `func TestGoExample${i + 1}_${sanitizeName(sourceName)}(t *testing.T) {\n\t// Source: ${block.source}:${block.loc.start.line}\n${indented}\n}\n`;
    })
    .join('\n');

  return `${header}${testCases}\n`;
}

function generatedFilePath(lang, name) {
  const ext = lang === 'typescript' ? 'ts' : lang === 'python' ? 'py' : 'go';
  return path.join(sdkTestsDir, lang, `generated_${ sanitizeName(name) }.${ext}`);
}

// 1. Build Soroban contract ABI + multi-language SDK type artifacts (#451)
const { abi, files: expectedFiles } = buildAllAbiArtifacts(repoRoot, { sdkTestsDir });

// 2. Extract MDX code examples with deterministic repo-relative POSIX paths
const files = getMdxFiles(docsDir);
const tsBlocks = [];
const pyBlocks = [];
const goBlocks = [];

for (const file of files) {
  const content = readFileSync(file, 'utf-8').replace(/\r\n/g, '\n');
  const relSource = path.relative(repoRoot, file).replace(/\\/g, '/');
  const blocks = extractCodeBlocks(content);
  for (const block of blocks) {
    const blockWithSource = { ...block, source: relSource };
    if (['typescript', 'ts', 'tsx'].includes(block.lang)) {
      tsBlocks.push(blockWithSource);
    } else if (block.lang === 'python') {
      pyBlocks.push(blockWithSource);
    } else if (block.lang === 'go') {
      goBlocks.push(blockWithSource);
    }
  }
}

const tsBySource = {};
const pyBySource = {};
const goBySource = {};

for (const block of tsBlocks) {
  const key = sanitizeName(block.source.replace(/^docs\/content\/en\/docs\//, '').replace(/\.mdx?$/, ''));
  if (!tsBySource[key]) tsBySource[key] = [];
  tsBySource[key].push(block);
}
for (const block of pyBlocks) {
  const key = sanitizeName(block.source.replace(/^docs\/content\/en\/docs\//, '').replace(/\.mdx?$/, ''));
  if (!pyBySource[key]) pyBySource[key] = [];
  pyBySource[key].push(block);
}
for (const block of goBlocks) {
  const key = sanitizeName(block.source.replace(/^docs\/content\/en\/docs\//, '').replace(/\.mdx?$/, ''));
  if (!goBySource[key]) goBySource[key] = [];
  goBySource[key].push(block);
}

if (tsBlocks.length > 0) {
  expectedFiles.set(generatedFilePath('typescript', 'sdk_examples'), generateTypeScriptTestFile(tsBlocks, 'all'));
}
if (pyBlocks.length > 0) {
  expectedFiles.set(generatedFilePath('python', 'sdk_examples'), generatePythonTestFile(pyBlocks, 'all'));
}
if (goBlocks.length > 0) {
  expectedFiles.set(generatedFilePath('go', 'sdk_examples'), generateGoTestFile(goBlocks, 'all'));
}

for (const [name, blocks] of Object.entries(tsBySource)) {
  expectedFiles.set(generatedFilePath('typescript', name), generateTypeScriptTestFile(blocks, name));
}
for (const [name, blocks] of Object.entries(pyBySource)) {
  expectedFiles.set(generatedFilePath('python', name), generatePythonTestFile(blocks, name));
}
for (const [name, blocks] of Object.entries(goBySource)) {
  expectedFiles.set(generatedFilePath('go', name), generateGoTestFile(blocks, name));
}

const result = syncGeneratedFiles(expectedFiles, { checkOnly: CHECK_ONLY, repoRoot });

if (CHECK_ONLY) {
  if (!result.inSync) {
    console.error(
      `✖ Generated ABI/SDK files are out of date (${result.drifted.length} drifted). Run \`node docs/scripts/generate-sdk-examples.mjs\`:\n` +
        result.drifted.map((f) => `  - ${f}`).join('\n')
    );
    process.exit(1);
  }
  console.log(
    `✓ All ${expectedFiles.size} generated ABI and SDK files are in sync (${abi.contracts.length} contracts).`
  );
  process.exit(0);
}

console.log(
  `Generated ABI for ${abi.contracts.length} Soroban contracts and extracted ${tsBlocks.length} TypeScript, ${pyBlocks.length} Python, ${goBlocks.length} Go examples (${result.written.length} files updated).`
);
console.log(`Output: ${sdkTestsDir}`);
