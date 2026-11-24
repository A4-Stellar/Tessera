import { readFileSync, writeFileSync, readdirSync, statSync, mkdirSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const docsDir = path.resolve(__dirname, '../content/en/docs');
const sdkTestsDir = path.resolve(__dirname, '../../sdk/tests/generated');
const contractsAbiDir = path.resolve(__dirname, '../../contracts/abi');

// Dynamically attempt importing remark/unist-util-visit with graceful regex fallback
let remark = null;
let visit = null;
try {
  const remarkModule = await import('remark');
  remark = remarkModule.remark;
  const visitModule = await import('unist-util-visit');
  visit = visitModule.visit;
} catch {
  // Graceful fallback when dependencies are not installed in docs/
}

function getMdxFiles(dir) {
  if (!existsSync(dir)) return [];
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
  if (remark && visit) {
    try {
      const tree = remark().parse(content);
      visit(tree, 'code', (node) => {
        const lang = node.lang ? node.lang.toLowerCase() : null;
        if (['typescript', 'ts', 'tsx', 'python', 'go'].includes(lang)) {
          blocks.push({ lang, value: node.value, loc: node.position });
        }
      });
      return blocks;
    } catch {
      // Fall through to regex fallback
    }
  }

  // Regex fallback: ```lang\n...```
  const regex = /```(typescript|ts|tsx|python|go)\b[^\n]*\n([\s\S]*?)```/gi;
  let match;
  while ((match = regex.exec(content)) !== null) {
    const rawLang = match[1].toLowerCase();
    const lang = rawLang.startsWith('ts') ? 'typescript' : rawLang;
    const value = match[2].trimEnd();
    const line = content.slice(0, match.index).split('\n').length;
    blocks.push({
      lang,
      value,
      loc: { start: { line } },
    });
  }
  return blocks;
}

function sanitizeName(name) {
  return name.replace(/[^a-zA-Z0-9_-]/g, '_').replace(/_+/g, '_').replace(/^_|_$/g, '');
}

function toPascalCase(str) {
  return str
    .replace(/[-_](\w)/g, (_, c) => c.toUpperCase())
    .replace(/^(\w)/, (_, c) => c.toUpperCase());
}

function toCamelCase(str) {
  const p = toPascalCase(str);
  return p.charAt(0).toLowerCase() + p.slice(1);
}

// ----------------------------------------------------------------------------
// Type Mappings for ABI -> SDK Generation (Issue #451 / #68)
// ----------------------------------------------------------------------------

function mapTsType(t) {
  if (!t || t === '()') return 'void';
  if (t === 'bool') return 'boolean';
  if (['u32', 'i32', 'u16', 'i16', 'u8', 'i8'].includes(t)) return 'number';
  if (['u64', 'i64', 'u128', 'i128'].includes(t)) return 'bigint | string';
  if (['Address', 'String', 'Symbol', 'str'].includes(t)) return 'string';
  if (t.startsWith('Bytes')) return 'Uint8Array | string';
  if (t.startsWith('Vec<')) {
    const inner = t.slice(4, -1);
    return `${mapTsType(inner)}[]`;
  }
  return 'any';
}

function mapPyType(t) {
  if (!t || t === '()') return 'None';
  if (t === 'bool') return 'bool';
  if (['u32', 'i32', 'u16', 'i16', 'u8', 'i8', 'u64', 'i64', 'u128', 'i128'].includes(t)) return 'int';
  if (['Address', 'String', 'Symbol', 'str'].includes(t)) return 'str';
  if (t.startsWith('Bytes')) return 'bytes';
  if (t.startsWith('Vec<')) {
    const inner = t.slice(4, -1);
    return `list[${mapPyType(inner)}]`;
  }
  return 'Any';
}

function mapGoType(t) {
  if (!t || t === '()') return '';
  if (t === 'bool') return 'bool';
  if (['u32', 'u16', 'u8'].includes(t)) return 'uint32';
  if (['i32', 'i16', 'i8'].includes(t)) return 'int32';
  if (['u64', 'i64', 'u128', 'i128'].includes(t)) return 'int64';
  if (['Address', 'String', 'Symbol', 'str'].includes(t)) return 'string';
  if (t.startsWith('Bytes')) return '[]byte';
  if (t.startsWith('Vec<')) {
    const inner = t.slice(4, -1);
    return `[]${mapGoType(inner) || 'any'}`;
  }
  return 'any';
}

function sanitizePyIdentifier(name) {
  const pyKeywords = new Set([
    'from', 'import', 'def', 'class', 'global', 'pass', 'return', 'yield',
    'raise', 'try', 'except', 'finally', 'type', 'is', 'in', 'not', 'and', 'or',
  ]);
  return pyKeywords.has(name) ? `${name}_` : name;
}

function sanitizeGoIdentifier(name) {
  return toPascalCase(name);
}

// ----------------------------------------------------------------------------
// SDK Contract Type Generators
// ----------------------------------------------------------------------------

function readContractAbis() {
  if (!existsSync(contractsAbiDir)) {
    return [];
  }
  const files = readdirSync(contractsAbiDir).filter((f) => f.endsWith('.abi.json')).sort();
  const abis = [];
  for (const f of files) {
    try {
      const content = readFileSync(path.join(contractsAbiDir, f), 'utf-8');
      abis.push(JSON.parse(content));
    } catch {
      // Ignore invalid files
    }
  }
  return abis;
}

function generateTypeScriptContractTypes(abis) {
  let out = `// Code generated by Tessera ABI -> SDK pipeline (Issue #451 / #68). DO NOT EDIT.\n`;
  out += `/**\n * Strongly-typed TypeScript interfaces and client declarations for Tessera Soroban contracts.\n */\n\n`;

  for (const abi of abis) {
    const contractName = toPascalCase(abi.contract);
    out += `// ----------------------------------------------------------------------------\n`;
    out += `// Contract: ${abi.contract}\n`;
    out += `// ----------------------------------------------------------------------------\n\n`;

    if (abi.moduleDoc) {
      out += `/**\n`;
      for (const line of abi.moduleDoc.split('\n')) {
        out += ` * ${line}\n`;
      }
      out += ` */\n`;
    }

    out += `export namespace ${contractName} {\n`;

    for (const m of abi.methods) {
      const methodName = toPascalCase(m.name);
      out += `  /**\n   * ${m.doc.replace(/\n/g, '\n   * ')}\n   */\n`;
      out += `  export interface ${methodName}Input {\n`;
      for (const inp of m.inputs) {
        out += `    ${inp.name}: ${mapTsType(inp.type)};\n`;
      }
      out += `  }\n`;
      out += `  export type ${methodName}Output = ${mapTsType(m.output)};\n\n`;
    }

    out += `  export interface Client {\n`;
    out += `    readonly contractId: string;\n`;
    for (const m of abi.methods) {
      const camel = toCamelCase(m.name);
      const pascal = toPascalCase(m.name);
      out += `    ${camel}(params: ${pascal}Input): Promise<${pascal}Output>;\n`;
    }
    out += `  }\n`;

    out += `}\n\n`;
  }

  return out;
}

function generatePythonContractTypes(abis) {
  let out = `# Code generated by Tessera ABI -> SDK pipeline (Issue #451 / #68). DO NOT EDIT.\n`;
  out += `from __future__ import annotations\n`;
  out += `from dataclasses import dataclass\n`;
  out += `from typing import Any, Optional, Union, List\n\n`;

  for (const abi of abis) {
    const contractName = toPascalCase(abi.contract);
    out += `# ============================================================================\n`;
    out += `# Contract: ${abi.contract}\n`;
    out += `# ============================================================================\n\n`;

    out += `class ${contractName}:\n`;
    if (abi.moduleDoc) {
      out += `    """\n`;
      for (const line of abi.moduleDoc.split('\n')) {
        out += `    ${line}\n`;
      }
      out += `    """\n\n`;
    }

    for (const m of abi.methods) {
      const methodName = toPascalCase(m.name);
      out += `    @dataclass\n`;
      out += `    class ${methodName}Input:\n`;
      out += `        """${m.doc.replace(/\n/g, ' ')}"""\n`;
      if (m.inputs.length === 0) {
        out += `        pass\n`;
      } else {
        for (const inp of m.inputs) {
          const pyName = sanitizePyIdentifier(inp.name);
          out += `        ${pyName}: ${mapPyType(inp.type)}\n`;
        }
      }
      out += `\n`;
    }

    out += `    class Client:\n`;
    out += `        def __init__(self, contract_id: str):\n`;
    out += `            self.contract_id = contract_id\n\n`;

    for (const m of abi.methods) {
      const pyMethodName = sanitizePyIdentifier(m.name);
      const pascal = toPascalCase(m.name);
      const retType = mapPyType(m.output);
      out += `        async def ${pyMethodName}(self, params: ${contractName}.${pascal}Input) -> ${retType}:\n`;
      out += `            """${m.doc.replace(/\n/g, ' ')}"""\n`;
      out += `            pass\n\n`;
    }

    out += `\n`;
  }

  return out;
}

function generateGoContractTypes(abis) {
  let out = `// Code generated by Tessera ABI -> SDK pipeline (Issue #451 / #68). DO NOT EDIT.\n`;
  out += `package generated\n\n`;
  out += `import "context"\n\n`;

  for (const abi of abis) {
    const contractName = toPascalCase(abi.contract);
    out += `// ============================================================================\n`;
    out += `// Contract: ${abi.contract}\n`;
    out += `// ============================================================================\n\n`;

    for (const m of abi.methods) {
      const methodName = toPascalCase(m.name);
      out += `// ${contractName}${methodName}Input contains input parameters for ${m.name}.\n`;
      out += `// ${m.doc.replace(/\n/g, ' ')}\n`;
      out += `type ${contractName}${methodName}Input struct {\n`;
      for (const inp of m.inputs) {
        const goName = sanitizeGoIdentifier(inp.name);
        out += `\t${goName} ${mapGoType(inp.type) || 'any'} \`json:"${inp.name}"\`\n`;
      }
      out += `}\n\n`;
    }

    out += `// ${contractName}Client defines the client interface for ${abi.contract}.\n`;
    out += `type ${contractName}Client interface {\n`;
    for (const m of abi.methods) {
      const methodName = toPascalCase(m.name);
      const retType = mapGoType(m.output);
      const retSig = retType ? `(${retType}, error)` : `error`;
      out += `\t${methodName}(ctx context.Context, input ${contractName}${methodName}Input) ${retSig}\n`;
    }
    out += `}\n\n`;
  }

  return out;
}

// ----------------------------------------------------------------------------
// MDX Test Generation (Preserved)
// ----------------------------------------------------------------------------

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

// ----------------------------------------------------------------------------
// Main Execution
// ----------------------------------------------------------------------------

// 1. Generate SDK types from ABI JSON files
console.log('=== Reading ABI JSON files & Generating SDK Types ===');
const abis = readContractAbis();
if (abis.length > 0) {
  const tsContent = generateTypeScriptContractTypes(abis);
  const tsPath = path.join(sdkTestsDir, 'typescript', 'contract_types.ts');
  if (!existsSync(path.dirname(tsPath))) mkdirSync(path.dirname(tsPath), { recursive: true });
  writeFileSync(tsPath, tsContent);
  console.log(`  ✓ Generated TypeScript contract types (${abis.length} contracts) -> ${tsPath}`);

  const pyContent = generatePythonContractTypes(abis);
  const pyPath = path.join(sdkTestsDir, 'python', 'contract_types.py');
  if (!existsSync(path.dirname(pyPath))) mkdirSync(path.dirname(pyPath), { recursive: true });
  writeFileSync(pyPath, pyContent);
  console.log(`  ✓ Generated Python contract dataclasses (${abis.length} contracts) -> ${pyPath}`);

  const goContent = generateGoContractTypes(abis);
  const goPath = path.join(sdkTestsDir, 'go', 'contract_types.go');
  if (!existsSync(path.dirname(goPath))) mkdirSync(path.dirname(goPath), { recursive: true });
  writeFileSync(goPath, goContent);
  console.log(`  ✓ Generated Go contract structs (${abis.length} contracts) -> ${goPath}`);
} else {
  console.log('  ! No ABI JSON files found in contracts/abi');
}

// 2. Extract code blocks from documentation MDX files
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

function sanitizeSourceKey(filePath) {
  const rel = path.relative(docsDir, filePath);
  return rel.replace(/[^a-zA-Z0-9_-]/g, '_').replace(/_+/g, '_').replace(/^_|_$/g, '');
}

for (const block of tsBlocks) {
  const key = sanitizeSourceKey(block.source);
  if (!tsBySource[key]) tsBySource[key] = [];
  tsBySource[key].push(block);
}
for (const block of pyBlocks) {
  const key = sanitizeSourceKey(block.source);
  if (!pyBySource[key]) pyBySource[key] = [];
  pyBySource[key].push(block);
}
for (const block of goBlocks) {
  const key = sanitizeSourceKey(block.source);
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
