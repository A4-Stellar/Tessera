import { readFileSync, writeFileSync, readdirSync, statSync, mkdirSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { remark } from 'remark';
import { visit } from 'unist-util-visit';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const docsDir = path.resolve(__dirname, '../content/en/docs');
const sdkTestsDir = path.resolve(__dirname, '../../sdk/tests/generated');

function getMdxFiles(dir) {
  let results = [];
  const list = readdirSync(dir);
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
  } catch (err) {
    // Skip unparseable files
  }
  return blocks;
}

function sanitizeName(name) {
  return name.replace(/[^a-zA-Z0-9_-]/g, '_').replace(/_+/g, '_').replace(/^_|_$/g, '');
}

function generateTypeScriptTestFile(blocks, sourceName) {
  const importStatements = `import { TesseraClient } from '@tessera/sdk';\nimport { describe, it, expect } from 'vitest';\n\nconst API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";\nconst client = new TesseraClient(API);\n\n`;

  const testCases = blocks.map((block, i) => {
    const code = block.value;
    return `  it('executes TypeScript example from ${sourceName} block ${i + 1}', async () => {\n    // Source: ${block.source}:${block.loc.start.line}\n${code.split('\n').map(l => `    ${l}`).join('\n')}\n  });`;
  }).join('\n\n');

  return `${importStatements}describe('SDK TypeScript Examples - ${sourceName}', () => {\n${testCases}\n});\n`;
}

function generatePythonTestFile(blocks, sourceName) {
  const header = `import pytest\nfrom tessera_sdk import TesseraClient\n\nAPI_BASE = "http://localhost:8080"\nclient = TesseraClient(API_BASE)\n\n`;

  const test_cases = blocks.map((block, i) => {
    const code = block.value;
    const indented = code.split('\n').map(l => `    ${l}`).join('\n');
    return `  async def test_python_example_${i + 1}_${sanitizeName(sourceName)}(self):\n    # Source: ${block.source}:${block.loc.start.line}\n${indented}\n`;
  }).join('\n');

  return `${header}class TestSDKPythonExamples:\n${test_cases}\n`;
}

function generateGoTestFile(blocks, sourceName) {
  const header = `package generated_test\n\nimport (\n\t"testing"\n\t"github.com/stretchr/testify/assert"\n)\n\nconst apiBase = "http://localhost:8080"\n\n`;

  const test_cases = blocks.map((block, i) => {
    const code = block.value;
    const indented = code.split('\n').map(l => `\t${l}`).join('\n');
    return `func TestGoExample${i + 1}_${sanitizeName(sourceName)}(t *testing.T) {\n\t// Source: ${block.source}:${block.loc.start.line}\n${indented}\n}\n`;
  }).join('\n');

  return `${header}${test_cases}\n`;
}

function writeGeneratedFile(lang, content, name) {
  const dir = path.join(sdkTestsDir, lang);
  if (!existsSync(dir)) {
    mkdirSync(dir, { recursive: true });
  }
  const ext = lang === 'typescript' ? 'ts' : lang === 'python' ? 'py' : 'go';
  const filePath = path.join(dir, `generated_${name}.${ext}`);
  writeFileSync(filePath, content);
  return filePath;
}

// Main execution
const files = getMdxFiles(docsDir);
const tsBlocks = [];
const pyBlocks = [];
const goBlocks = [];

for (const file of files) {
  const content = readFileSync(file, 'utf-8');
  const blocks = extractCodeBlocks(content);
  for (const block of blocks) {
    const blockWithSource = { ...block, source: file };
    if (['typescript', 'ts', 'tsx'].includes(block.lang)) {
      tsBlocks.push(blockWithSource);
    } else if (block.lang === 'python') {
      pyBlocks.push(blockWithSource);
    } else if (block.lang === 'go') {
      goBlocks.push(blockWithSource);
    }
  }
}

// Group by source file for individual generated files
const tsBySource = {};
const pyBySource = {};
const goBySource = {};

for (const block of tsBlocks) {
  const key = block.source.replace(/\//g, '_').replace(/\.mdx$/, '');
  if (!tsBySource[key]) tsBySource[key] = [];
  tsBySource[key].push(block);
}
for (const block of pyBlocks) {
  const key = block.source.replace(/\//g, '_').replace(/\.mdx$/, '');
  if (!pyBySource[key]) pyBySource[key] = [];
  pyBySource[key].push(block);
}
for (const block of goBlocks) {
  const key = block.source.replace(/\//g, '_').replace(/\.mdx$/, '');
  if (!goBySource[key]) goBySource[key] = [];
  goBySource[key].push(block);
}

// Generate consolidated files
if (tsBlocks.length > 0) {
  const content = generateTypeScriptTestFile(tsBlocks, 'all');
  writeGeneratedFile('typescript', content, 'sdk_examples');
  console.log(`Generated TypeScript test file with ${tsBlocks.length} examples`);
}
if (pyBlocks.length > 0) {
  const content = generatePythonTestFile(pyBlocks, 'all');
  writeGeneratedFile('python', content, 'sdk_examples');
  console.log(`Generated Python test file with ${pyBlocks.length} examples`);
}
if (goBlocks.length > 0) {
  const content = generateGoTestFile(goBlocks, 'all');
  writeGeneratedFile('go', content, 'sdk_examples');
  console.log(`Generated Go test file with ${goBlocks.length} examples`);
}

// Generate per-source files
for (const [name, blocks] of Object.entries(tsBySource)) {
  const content = generateTypeScriptTestFile(blocks, name);
  writeGeneratedFile('typescript', content, name);
}
for (const [name, blocks] of Object.entries(pyBySource)) {
  const content = generatePythonTestFile(blocks, name);
  writeGeneratedFile('python', content, name);
}
for (const [name, blocks] of Object.entries(goBySource)) {
  const content = generateGoTestFile(blocks, name);
  writeGeneratedFile('go', content, name);
}

console.log(`\nTotal: ${tsBlocks.length} TypeScript, ${pyBlocks.length} Python, ${goBlocks.length} Go examples extracted`);
console.log(`Output: ${sdkTestsDir}`);
