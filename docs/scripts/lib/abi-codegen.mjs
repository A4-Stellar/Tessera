/**
 * Soroban Contract ABI Extractor & Multi-Language SDK Type Codegen (Issue #451).
 *
 * Extracts contract interfaces as a canonical JSON ABI (supporting
 * `soroban contract inspect --wasm` when compiled WASM binaries and the Soroban
 * CLI are present, plus deterministic Rust source extraction with `//!` module
 * and `///` item doc comments) and generates:
 *   - Rust `#![no_std]` ABI bindings (`contracts/common/src/abi.rs`)
 *   - TypeScript interfaces & method input/output types (`sdk/tests/generated/typescript/contract_types.ts`)
 *   - Python `@dataclass` & `IntEnum` definitions (`sdk/tests/generated/python/contract_types.py`)
 *   - Go structs & error constants (`sdk/tests/generated/go/contract_types.go`)
 *   - Canonical JSON ABI (`sdk/tests/generated/contracts_abi.json`)
 */

import { execSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';

export const CORE_CONTRACTS = Object.freeze([
  {
    contractId: 'registry',
    rustModule: 'registry',
    pascalPrefix: 'Registry',
    contractName: 'RegistryContract',
    sourceRelPath: 'contracts/registry/src/lib.rs',
    wasmName: 'tessera_registry.wasm',
  },
  {
    contractId: 'compliance',
    rustModule: 'compliance',
    pascalPrefix: 'Compliance',
    contractName: 'ComplianceContract',
    sourceRelPath: 'contracts/compliance/src/lib.rs',
    wasmName: 'tessera_compliance.wasm',
  },
  {
    contractId: 'dividend',
    rustModule: 'dividend',
    pascalPrefix: 'Dividend',
    contractName: 'DividendContract',
    sourceRelPath: 'contracts/dividend/src/lib.rs',
    wasmName: 'tessera_dividend.wasm',
  },
  {
    contractId: 'asset-token',
    rustModule: 'asset_token',
    pascalPrefix: 'AssetToken',
    contractName: 'AssetTokenContract',
    sourceRelPath: 'contracts/asset-token/src/lib.rs',
    wasmName: 'tessera_asset_token.wasm',
  },
]);

export function toPascalCase(value) {
  return String(value ?? '')
    .split(/[^a-zA-Z0-9]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join('');
}

export function toCamelCase(value) {
  const pascal = toPascalCase(value);
  return pascal ? pascal.charAt(0).toLowerCase() + pascal.slice(1) : '';
}

function cleanRustType(rawType) {
  return String(rawType ?? '')
    .replace(/\s+/g, ' ')
    .trim();
}

/**
 * Extracts `//!` module-level documentation and any per-method notes mentioned
 * in `//!` headers.
 */
export function extractModuleDoc(source) {
  const moduleLines = [];
  const methodMentions = new Map();

  for (const rawLine of String(source).split(/\r?\n/)) {
    const trimmed = rawLine.trim();
    if (trimmed.startsWith('//!')) {
      const content = trimmed.replace(/^\/\/!\s?/, '').trim();
      moduleLines.push(content);
      // Capture patterns like `method_name`: description or - `method_name` — description
      const mentionMatch = /^(?:[-*]\s*)?`([a-z0-9_]+)`\s*(?::|—|-)\s*(.+)$/i.exec(content);
      if (mentionMatch) {
        methodMentions.set(mentionMatch[1], mentionMatch[2].trim());
      }
    } else if (moduleLines.length > 0 && trimmed !== '') {
      break;
    }
  }

  const paragraphs = [];
  let current = [];
  for (const line of moduleLines) {
    if (!line) {
      if (current.length > 0) {
        paragraphs.push(current.join(' '));
        current = [];
      }
    } else {
      current.push(line);
    }
  }
  if (current.length > 0) paragraphs.push(current.join(' '));

  return {
    moduleDoc: paragraphs.join('\n\n'),
    summaryDoc: paragraphs[0] || '',
    methodMentions,
  };
}

/** Collects contiguous `///` doc comments immediately preceding line index `lineIdx`. */
function collectPrecedingDocComments(lines, lineIdx) {
  const collected = [];
  for (let i = lineIdx - 1; i >= 0; i--) {
    const trimmed = lines[i].trim();
    if (trimmed.startsWith('///')) {
      collected.unshift(trimmed.replace(/^\/\/\/\s?/, '').trim());
    } else if (trimmed.startsWith('#[') || trimmed === '') {
      if (trimmed === '' && collected.length > 0) break;
      continue;
    } else {
      break;
    }
  }
  return collected.filter(Boolean).join(' ');
}

function humanizeMethodDoc(methodName, contractId, inputs, output, existingDoc, moduleMentions) {
  if (existingDoc) return existingDoc;
  if (moduleMentions && moduleMentions.has(methodName)) {
    return moduleMentions.get(methodName);
  }
  const paramList = inputs.length > 0 ? inputs.map((p) => p.name).join(', ') : 'no parameters';
  const returnSuffix = output && output !== 'void' ? ` Returns \`${output}\`.` : '';
  return `Invokes \`${contractId}::${methodName}\` (${paramList}).${returnSuffix}`;
}

/**
 * Parses a Soroban Rust contract source file (`src/lib.rs`) into a structured
 * contract ABI object with `//!` and `///` documentation.
 */
export function extractContractAbiFromSource(
  source,
  { id, contractId = id || 'contract', contractName = 'Contract', pascalPrefix = toPascalCase(contractId) } = {}
) {
  const lines = String(source).split(/\r?\n/);
  const { moduleDoc, summaryDoc, methodMentions } = extractModuleDoc(source);

  const structs = [];
  const enums = [];
  const errors = [];
  const methods = [];

  // 1. Parse #[contracttype] structs & enums and #[contracterror] enums
  let hasContractType = false;
  let hasContractError = false;

  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (trimmed.startsWith('#[contracttype]')) {
      hasContractType = true;
      continue;
    }
    if (trimmed.startsWith('#[contracterror]')) {
      hasContractError = true;
      continue;
    }
    if (trimmed.startsWith('#[')) continue;

    if (hasContractType) {
      const structMatch = /^(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z0-9_]+)\s*\{/.exec(trimmed);
      const enumMatch = /^(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z0-9_]+)\s*\{/.exec(trimmed);

      if (structMatch) {
        hasContractType = false;
        const structName = structMatch[1];
        const structDoc = collectPrecedingDocComments(lines, i) || `${structName} contract data structure.`;
        const fields = [];
        let j = i + 1;
        while (j < lines.length && !lines[j].trim().startsWith('}')) {
          const fieldLine = lines[j].trim();
          const fieldMatch = /^(?:pub\s+)?([a-zA-Z0-9_]+)\s*:\s*([^,]+),?$/.exec(fieldLine);
          if (fieldMatch) {
            fields.push({
              name: fieldMatch[1],
              type: cleanRustType(fieldMatch[2]),
              doc: collectPrecedingDocComments(lines, j),
            });
          }
          j++;
        }
        structs.push({ name: structName, doc: structDoc, fields });
        i = j;
        continue;
      }

      if (enumMatch) {
        hasContractType = false;
        const enumName = enumMatch[1];
        const isPublic = trimmed.startsWith('pub ') && !trimmed.startsWith('pub(');
        const enumDoc = collectPrecedingDocComments(lines, i) || `${enumName} contract enumeration.`;
        const variants = [];
        let j = i + 1;
        while (j < lines.length && !lines[j].trim().startsWith('}')) {
          const vLine = lines[j].trim();
          if (vLine && !vLine.startsWith('//') && !vLine.startsWith('#[')) {
            const vMatch = /^([A-Za-z0-9_]+)(?:\(([^)]*)\))?(?:\s*=\s*(\d+))?\s*,?$/.exec(vLine);
            if (vMatch) {
              const variantObj = {
                name: vMatch[1],
                tupleTypes: vMatch[2]
                  ? vMatch[2]
                      .split(',')
                      .map((t) => cleanRustType(t))
                      .filter(Boolean)
                  : [],
                doc: collectPrecedingDocComments(lines, j),
              };
              if (vMatch[3] !== undefined) {
                variantObj.value = Number(vMatch[3]);
              }
              variants.push(variantObj);
            }
          }
          j++;
        }
        if (enumName !== 'DataKey' && isPublic) {
          enums.push({ name: enumName, doc: enumDoc, variants });
        }
        i = j;
        continue;
      }

      if (trimmed !== '') hasContractType = false;
    }

    if (hasContractError) {
      const errEnumMatch = /^(?:pub\s+)?enum\s+([A-Za-z0-9_]+)\s*\{/.exec(trimmed);
      if (errEnumMatch) {
        hasContractError = false;
        let j = i + 1;
        while (j < lines.length && !lines[j].trim().startsWith('}')) {
          const eLine = lines[j].trim();
          const eMatch = /^([A-Za-z0-9_]+)\s*=\s*(\d+)\s*,?$/.exec(eLine);
          if (eMatch) {
            errors.push({
              name: eMatch[1],
              code: Number(eMatch[2]),
              doc: collectPrecedingDocComments(lines, j) || `${eMatch[1]} contract error (code ${eMatch[2]}).`,
            });
          }
          j++;
        }
        i = j;
        continue;
      }
      if (trimmed !== '') hasContractError = false;
    }
  }

  // 2. Parse #[contractimpl] impl blocks
  for (let i = 0; i < lines.length; i++) {
    if (!lines[i].trim().startsWith('#[contractimpl]')) continue;
    let implLineIdx = i + 1;
    while (implLineIdx < lines.length && !/\bimpl\b/.test(lines[implLineIdx])) implLineIdx++;
    if (implLineIdx >= lines.length) continue;

    // Scan balanced braces of the impl block
    let braceDepth = 0;
    let started = false;
    for (let j = implLineIdx; j < lines.length; j++) {
      const line = lines[j];
      for (const ch of line) {
        if (ch === '{') {
          braceDepth++;
          started = true;
        } else if (ch === '}') {
          braceDepth--;
        }
      }

      const trimmed = line.trim();
      const fnStartMatch = /^pub\s+fn\s+([a-zA-Z0-9_]+)\s*\(/.exec(trimmed);
      if (fnStartMatch && braceDepth >= 1) {
        const fnName = fnStartMatch[1];
        const rawDoc = collectPrecedingDocComments(lines, j);

        // Collect signature lines up to the opening `{` of the function body
        let sigLines = [trimmed];
        let k = j;
        while (k < lines.length) {
          const checkLine = k === j ? trimmed : lines[k].trim();
          // Find `{` that opens the function body (after closing `)`)
          const closeParenIdx = sigLines.join(' ').indexOf(')');
          if (closeParenIdx !== -1 && checkLine.includes('{')) {
            break;
          }
          k++;
          if (k < lines.length) sigLines.push(lines[k].trim());
        }

        const fullSig = sigLines.join(' ').replace(/\{[\s\S]*$/, '').trim();
        const sigMatch = /^pub\s+fn\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)(?:\s*->\s*(.+))?$/.exec(fullSig);
        if (sigMatch) {
          const rawParams = sigMatch[2]
            .split(',')
            .map((p) => p.trim())
            .filter(Boolean);
          const inputs = [];
          for (let pIdx = 0; pIdx < rawParams.length; pIdx++) {
            const colonIdx = rawParams[pIdx].indexOf(':');
            if (colonIdx === -1) continue;
            const pName = rawParams[pIdx].slice(0, colonIdx).trim();
            const pType = cleanRustType(rawParams[pIdx].slice(colonIdx + 1));
            if (pIdx === 0 && (pType === 'Env' || pType === '&Env' || pName === 'env' || pName === '_env')) {
              continue;
            }
            inputs.push({ name: pName.replace(/^_/, ''), type: pType });
          }
          let output = sigMatch[3] ? cleanRustType(sigMatch[3]) : 'void';
          const resultMatch = /^Result<(.+)>$/.exec(output);
          if (resultMatch) {
            const innerParts = splitTopLevelComma(resultMatch[1]);
            const okType = (innerParts[0] || '').trim();
            output = !okType || okType === '()' ? 'void' : okType;
          }
          const doc = humanizeMethodDoc(fnName, contractId, inputs, output, rawDoc, methodMentions);
          methods.push({ name: fnName, doc, inputs, output });
        }
      }

      if (started && braceDepth <= 0) break;
    }
  }

  return {
    contractId,
    contractName,
    pascalPrefix,
    doc: moduleDoc || summaryDoc || `${contractName} Soroban smart contract.`,
    summaryDoc: summaryDoc || `${contractName} Soroban smart contract.`,
    structs,
    enums,
    errors,
    methods,
  };
}

/**
 * Parses the output of `soroban contract inspect --wasm <file>` (either JSON
 * or key-value text output) into method/struct/error specifications.
 */
export function parseSorobanInspectOutput(outputText) {
  const trimmed = String(outputText ?? '').trim();
  if (!trimmed) return { methods: [], structs: [], errors: [] };

  if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
    try {
      const parsed = JSON.parse(trimmed);
      const entries = Array.isArray(parsed) ? parsed : parsed.entries || parsed.functions || [];
      const methods = [];
      const structs = [];
      const errors = [];
      for (const entry of entries) {
        if (entry.type === 'function' || entry.kind === 'function' || entry.inputs) {
          const rawOut = entry.output || (entry.outputs && entry.outputs[0]) || 'void';
          const normalizedOut = typeof rawOut === 'string' ? rawOut : rawOut?.type || 'void';
          methods.push({
            name: entry.name,
            doc: entry.doc || '',
            inputs: (entry.inputs || []).map((inp) => ({ name: inp.name, type: inp.type })),
            output: normalizedOut,
          });
        } else if (entry.type === 'struct' || entry.fields) {
          structs.push({
            name: entry.name,
            doc: entry.doc || '',
            fields: (entry.fields || []).map((f) => ({ name: f.name, type: f.type, doc: f.doc || '' })),
          });
        } else if (entry.type === 'error' || typeof entry.code === 'number') {
          errors.push({
            name: entry.name,
            code: Number(entry.code ?? entry.value ?? 0),
            doc: entry.doc || '',
          });
        }
      }
      return { methods, structs, errors };
    } catch {
      // Fall through to text parsing
    }
  }

  // Parse human-readable `soroban contract inspect` text output lines like `fn register_asset(issuer: Address) -> u64`
  const methods = [];
  for (const line of trimmed.split(/\r?\n/)) {
    const fnMatch = /^\s*(?:pub\s+)?fn\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)(?:\s*->\s*(.+))?$/.exec(line);
    if (fnMatch) {
      const inputs = fnMatch[2]
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean)
        .map((pair) => {
          const [name, type] = pair.split(':').map((p) => p.trim());
          return { name, type: type || 'Val' };
        })
        .filter((inp, idx) => !(idx === 0 && (inp.type === 'Env' || inp.name === 'env')));
      methods.push({
        name: fnMatch[1],
        doc: '',
        inputs,
        output: fnMatch[3] ? fnMatch[3].trim() : 'void',
      });
    }
  }
  return { methods, structs: [], errors: [] };
}

/**
 * Extracts a single contract's ABI using `soroban contract inspect --wasm`
 * when a WASM binary and `soroban` CLI are available, enriched with `//!` and
 * `///` documentation from the Rust source file.
 */
export function extractContractAbi({
  repoRoot,
  contractSpec,
  contractId = contractSpec?.id,
  contractName = contractSpec?.contractName,
  pascalPrefix = contractSpec?.pascalPrefix || toPascalCase(contractId),
  sourcePath = contractSpec && repoRoot ? path.join(repoRoot, contractSpec.relativePath) : undefined,
  wasmPath,
  execSyncImpl = execSync,
}) {
  const source = readFileSync(sourcePath, 'utf8');
  const fromSource = extractContractAbiFromSource(source, { contractId, contractName, pascalPrefix });

  if (wasmPath && (existsSync(wasmPath) || execSyncImpl !== execSync)) {
    try {
      const stdout = execSyncImpl(`soroban contract inspect --wasm "${wasmPath}"`, {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
      });
      const inspected = parseSorobanInspectOutput(stdout);
      if (inspected.methods.length > 0) {
        const sourceMethodMap = new Map(fromSource.methods.map((m) => [m.name, m]));
        for (const inspectedFn of inspected.methods) {
          const existing = sourceMethodMap.get(inspectedFn.name);
          if (existing) {
            if (!existing.doc && inspectedFn.doc) existing.doc = inspectedFn.doc;
          } else {
            fromSource.methods.push({
              ...inspectedFn,
              doc: inspectedFn.doc || `Invokes \`${contractId}::${inspectedFn.name}\`.`,
            });
          }
        }
      }
    } catch {
      // Soroban CLI not installed or WASM not yet built; deterministic source extraction is authoritative.
    }
  }

  return fromSource;
}

/**
 * Extracts the complete workspace ABI JSON across all four core Tessera
 * Soroban contracts (`registry`, `compliance`, `dividend`, `asset-token`).
 */
export function extractWorkspaceAbi(repoRoot, options = {}) {
  const contracts = CORE_CONTRACTS.map((meta) => {
    const sourcePath = path.join(repoRoot, meta.sourceRelPath);
    const candidateWasm = path.join(
      repoRoot,
      'contracts',
      'target',
      'wasm32-unknown-unknown',
      'release',
      meta.wasmName
    );
    return extractContractAbi({
      contractId: meta.contractId,
      contractName: meta.contractName,
      pascalPrefix: meta.pascalPrefix,
      sourcePath,
      wasmPath: options.wasmDir ? path.join(options.wasmDir, meta.wasmName) : candidateWasm,
      execSyncImpl: options.execSyncImpl,
    });
  });

  return {
    schemaVersion: '1.0.0',
    generator: 'soroban-contract-abi-pipeline',
    contracts,
  };
}

// ── Type mapping helpers (Rust/Soroban -> TypeScript, Python, Go) ───────────

function splitTopLevelComma(inner) {
  const parts = [];
  let depth = 0;
  let current = '';
  for (const ch of inner) {
    if (ch === '<' || ch === '(') depth++;
    else if (ch === '>' || ch === ')') depth--;
    if (ch === ',' && depth === 0) {
      parts.push(current.trim());
      current = '';
    } else {
      current += ch;
    }
  }
  if (current.trim()) parts.push(current.trim());
  return parts;
}

export function rustTypeToTs(rustType, pascalPrefix = '', knownTypes = new Set()) {
  const t = cleanRustType(rustType).replace(/^[a-z0-9_]+::/i, '');
  if (!t || t === 'void' || t === '()') return 'void';
  if (t === 'bool') return 'boolean';
  if (['u32', 'i32', 'u64', 'usize'].includes(t)) return 'number';
  if (['i64', 'u128', 'i128', 'U256', 'I256'].includes(t)) return 'bigint | string';
  if (['Address', 'String', 'Symbol', 'Bytes', 'Val'].includes(t) || /^BytesN<\d+>$/.test(t)) return 'string';
  const vecMatch = /^Vec<(.+)>$/.exec(t);
  if (vecMatch) return `Array<${rustTypeToTs(vecMatch[1], pascalPrefix, knownTypes)}>`;
  const optMatch = /^Option<(.+)>$/.exec(t);
  if (optMatch) return `${rustTypeToTs(optMatch[1], pascalPrefix, knownTypes)} | null`;
  if (t.startsWith('(') && t.endsWith(')')) {
    const elems = splitTopLevelComma(t.slice(1, -1));
    return `[${elems.map((e) => rustTypeToTs(e, pascalPrefix, knownTypes)).join(', ')}]`;
  }
  if (knownTypes.has(t)) return `${pascalPrefix}${t}`;
  return 'unknown';
}

export function rustTypeToPy(rustType, pascalPrefix = '', knownTypes = new Set()) {
  const t = cleanRustType(rustType).replace(/^[a-z0-9_]+::/i, '');
  if (!t || t === 'void' || t === '()') return 'None';
  if (t === 'bool') return 'bool';
  if (['u32', 'i32', 'u64', 'i64', 'u128', 'i128', 'usize'].includes(t)) return 'int';
  if (['Address', 'String', 'Symbol', 'Bytes', 'Val'].includes(t) || /^BytesN<\d+>$/.test(t)) return 'str';
  const vecMatch = /^Vec<(.+)>$/.exec(t);
  if (vecMatch) return `List[${rustTypeToPy(vecMatch[1], pascalPrefix, knownTypes)}]`;
  const optMatch = /^Option<(.+)>$/.exec(t);
  if (optMatch) return `Optional[${rustTypeToPy(optMatch[1], pascalPrefix, knownTypes)}]`;
  if (t.startsWith('(') && t.endsWith(')')) {
    const elems = splitTopLevelComma(t.slice(1, -1));
    return `Tuple[${elems.map((e) => rustTypeToPy(e, pascalPrefix, knownTypes)).join(', ')}]`;
  }
  if (knownTypes.has(t)) return `${pascalPrefix}${t}`;
  return 'Any';
}

export function rustTypeToGo(rustType, pascalPrefix = '', knownTypes = new Set()) {
  const t = cleanRustType(rustType).replace(/^[a-z0-9_]+::/i, '');
  if (!t || t === 'void' || t === '()') return 'struct{}';
  if (t === 'bool') return 'bool';
  if (t === 'u32') return 'uint32';
  if (t === 'i32') return 'int32';
  if (t === 'u64') return 'uint64';
  if (t === 'i64') return 'int64';
  if (['u128', 'i128', 'Address', 'String', 'Symbol', 'Bytes', 'Val'].includes(t) || /^BytesN<\d+>$/.test(t)) {
    return 'string';
  }
  const vecMatch = /^Vec<(.+)>$/.exec(t);
  if (vecMatch) return `[]${rustTypeToGo(vecMatch[1], pascalPrefix, knownTypes)}`;
  const optMatch = /^Option<(.+)>$/.exec(t);
  if (optMatch) return `*${rustTypeToGo(optMatch[1], pascalPrefix, knownTypes)}`;
  if (t.startsWith('(') && t.endsWith(')')) {
    return '[]interface{}';
  }
  if (knownTypes.has(t)) return `${pascalPrefix}${t}`;
  return 'interface{}';
}

// ── 1. Rust Bindings Generator (contracts/common/src/abi.rs) ────────────────

function escapeRustString(str) {
  return String(str ?? '')
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"')
    .replace(/\r?\n/g, ' ');
}

export function generateRustBindings(abi) {
  const lines = [
    '//! Auto-generated Soroban contract ABI descriptors and `#![no_std]` Rust type',
    '//! bindings for the Tessera contract suite (Issue #451).',
    '//!',
    '//! Generated by `contracts/build.rs` and `docs/scripts/generate-sdk-examples.mjs`.',
    '//! Do not edit by hand.',
    '',
    '#[derive(Clone, Copy, Debug, Eq, PartialEq)]',
    'pub struct ParamAbiSpec {',
    "    pub name: &'static str,",
    "    pub rust_type: &'static str,",
    '}',
    '',
    '#[derive(Clone, Copy, Debug, Eq, PartialEq)]',
    'pub struct MethodAbiSpec {',
    "    pub name: &'static str,",
    "    pub doc: &'static str,",
    "    pub inputs: &'static [ParamAbiSpec],",
    "    pub output: &'static str,",
    '}',
    '',
    '#[derive(Clone, Copy, Debug, Eq, PartialEq)]',
    'pub struct FieldAbiSpec {',
    "    pub name: &'static str,",
    "    pub rust_type: &'static str,",
    '}',
    '',
    '#[derive(Clone, Copy, Debug, Eq, PartialEq)]',
    'pub struct StructAbiSpec {',
    "    pub name: &'static str,",
    "    pub doc: &'static str,",
    "    pub fields: &'static [FieldAbiSpec],",
    '}',
    '',
    '#[derive(Clone, Copy, Debug, Eq, PartialEq)]',
    'pub struct ErrorAbiSpec {',
    "    pub name: &'static str,",
    '    pub code: u32,',
    "    pub doc: &'static str,",
    '}',
    '',
    '#[derive(Clone, Copy, Debug, Eq, PartialEq)]',
    'pub struct ContractAbiSpec {',
    "    pub id: &'static str,",
    "    pub name: &'static str,",
    "    pub doc: &'static str,",
    "    pub methods: &'static [MethodAbiSpec],",
    "    pub structs: &'static [StructAbiSpec],",
    "    pub errors: &'static [ErrorAbiSpec],",
    '}',
    '',
  ];

  const moduleNames = [];
  for (const contract of abi.contracts) {
    const modName = contract.contractId.replace(/-/g, '_');
    moduleNames.push(modName);

    lines.push(`pub mod ${modName} {`);
    lines.push('    use super::*;');
    lines.push('');
    lines.push(`    pub const CONTRACT_ID: &str = "${escapeRustString(contract.contractId)}";`);
    lines.push(`    pub const CONTRACT_NAME: &str = "${escapeRustString(contract.contractName)}";`);
    lines.push(`    pub const CONTRACT_DOC: &str = "${escapeRustString(contract.doc)}";`);
    lines.push('');

    lines.push('    pub const STRUCTS: &[StructAbiSpec] = &[');
    for (const st of contract.structs) {
      lines.push('        StructAbiSpec {');
      lines.push(`            name: "${escapeRustString(st.name)}",`);
      lines.push(`            doc: "${escapeRustString(st.doc)}",`);
      lines.push('            fields: &[');
      for (const f of st.fields) {
        lines.push(
          `                FieldAbiSpec { name: "${escapeRustString(f.name)}", rust_type: "${escapeRustString(f.type)}" },`
        );
      }
      lines.push('            ],');
      lines.push('        },');
    }
    lines.push('    ];');
    lines.push('');

    lines.push('    pub const ERRORS: &[ErrorAbiSpec] = &[');
    for (const err of contract.errors) {
      lines.push(
        `        ErrorAbiSpec { name: "${escapeRustString(err.name)}", code: ${err.code}, doc: "${escapeRustString(err.doc)}" },`
      );
    }
    lines.push('    ];');
    lines.push('');

    lines.push('    pub const METHODS: &[MethodAbiSpec] = &[');
    for (const fn of contract.methods) {
      lines.push('        MethodAbiSpec {');
      lines.push(`            name: "${escapeRustString(fn.name)}",`);
      lines.push(`            doc: "${escapeRustString(fn.doc)}",`);
      lines.push('            inputs: &[');
      for (const inp of fn.inputs) {
        lines.push(
          `                ParamAbiSpec { name: "${escapeRustString(inp.name)}", rust_type: "${escapeRustString(inp.type)}" },`
        );
      }
      lines.push('            ],');
      lines.push(`            output: "${escapeRustString(fn.output)}",`);
      lines.push('        },');
    }
    lines.push('    ];');
    lines.push('');

    lines.push('    pub const CONTRACT_ABI: ContractAbiSpec = ContractAbiSpec {');
    lines.push('        id: CONTRACT_ID,');
    lines.push('        name: CONTRACT_NAME,');
    lines.push('        doc: CONTRACT_DOC,');
    lines.push('        methods: METHODS,');
    lines.push('        structs: STRUCTS,');
    lines.push('        errors: ERRORS,');
    lines.push('    };');
    lines.push('}');
    lines.push('');
  }

  lines.push('pub const ALL_CONTRACTS: &[ContractAbiSpec] = &[');
  for (const modName of moduleNames) {
    lines.push(`    ${modName}::CONTRACT_ABI,`);
  }
  lines.push('];');
  lines.push('');

  return lines.join('\n');
}

// ── 2. TypeScript Types Generator ───────────────────────────────────────────

export function generateTypeScriptTypes(abi) {
  const lines = [
    '// Auto-generated by docs/scripts/generate-sdk-examples.mjs from Soroban contract ABIs (#451).',
    '// Do not edit by hand.',
    '',
  ];

  for (const contract of abi.contracts) {
    const prefix = contract.pascalPrefix || toPascalCase(contract.contractId);
    const knownTypes = new Set([
      ...contract.structs.map((s) => s.name),
      ...contract.enums.map((e) => e.name),
    ]);

    lines.push('/**');
    for (const docLine of contract.doc.split('\n')) {
      lines.push(` * ${docLine}`);
    }
    lines.push(' */');
    lines.push(`export const ${prefix.toUpperCase()}_CONTRACT_DOC = ${JSON.stringify(contract.doc)} as const;`);
    lines.push('');

    for (const en of contract.enums) {
      lines.push(`/** ${en.doc} */`);
      const union = en.variants.map((v) => JSON.stringify(v.name)).join(' | ') || 'string';
      lines.push(`export type ${prefix}${en.name} = ${union};`);
      lines.push('');
    }

    for (const st of contract.structs) {
      lines.push(`/** ${st.doc} */`);
      lines.push(`export interface ${prefix}${st.name} {`);
      for (const f of st.fields) {
        if (f.doc) lines.push(`  /** ${f.doc} */`);
        lines.push(`  ${f.name}: ${rustTypeToTs(f.type, prefix, knownTypes)};`);
      }
      lines.push('}');
      lines.push('');
    }

    if (contract.errors.length > 0) {
      lines.push(`/** Error codes for ${contract.contractName}. */`);
      lines.push(`export const ${prefix}ErrorCode = {`);
      for (const err of contract.errors) {
        lines.push(`  /** ${err.doc} */`);
        lines.push(`  ${err.name}: ${err.code},`);
      }
      lines.push('} as const;');
      lines.push(`export type ${prefix}ErrorCodeValue = (typeof ${prefix}ErrorCode)[keyof typeof ${prefix}ErrorCode];`);
      lines.push('');
    }

    for (const fn of contract.methods) {
      const fnPascal = toPascalCase(fn.name);
      lines.push(`/** ${fn.doc} */`);
      lines.push(`export interface ${prefix}${fnPascal}Input {`);
      for (const inp of fn.inputs) {
        lines.push(`  ${inp.name}: ${rustTypeToTs(inp.type, prefix, knownTypes)};`);
      }
      lines.push('}');
      lines.push(
        `export type ${prefix}${fnPascal}Output = ${rustTypeToTs(fn.output, prefix, knownTypes)};`
      );
      lines.push('');
    }

    lines.push(`/** Typed method interface for ${contract.contractName}. */`);
    lines.push(`export interface ${prefix}ContractAbi {`);
    for (const fn of contract.methods) {
      const fnPascal = toPascalCase(fn.name);
      lines.push(`  /** ${fn.doc} */`);
      lines.push(`  ${fn.name}(input: ${prefix}${fnPascal}Input): Promise<${prefix}${fnPascal}Output>;`);
    }
    lines.push('}');
    lines.push('');
  }

  return lines.join('\n');
}

// ── 3. Python Dataclasses Generator ─────────────────────────────────────────

export function generatePythonDataclasses(abi) {
  const lines = [
    '"""Auto-generated by docs/scripts/generate-sdk-examples.mjs from Soroban contract ABIs (#451).',
    'Do not edit by hand.',
    '"""',
    'from __future__ import annotations',
    '',
    'from dataclasses import dataclass',
    'from enum import Enum, IntEnum',
    'from typing import Any, List, Optional, Tuple',
    '',
  ];

  for (const contract of abi.contracts) {
    const prefix = contract.pascalPrefix || toPascalCase(contract.contractId);
    const knownTypes = new Set([
      ...contract.structs.map((s) => s.name),
      ...contract.enums.map((e) => e.name),
    ]);

    lines.push(`${prefix.toUpperCase()}_CONTRACT_DOC = ${JSON.stringify(contract.doc)}`);
    lines.push('');

    for (const en of contract.enums) {
      lines.push(`class ${prefix}${en.name}(str, Enum):`);
      lines.push(`    """${en.doc.replace(/"/g, '\\"')}"""`);
      if (en.variants.length === 0) {
        lines.push('    pass');
      } else {
        for (const v of en.variants) {
          lines.push(`    ${v.name} = ${JSON.stringify(v.name)}`);
        }
      }
      lines.push('');
    }

    for (const st of contract.structs) {
      lines.push('@dataclass(frozen=True)');
      lines.push(`class ${prefix}${st.name}:`);
      lines.push(`    """${st.doc.replace(/"/g, '\\"')}"""`);
      if (st.fields.length === 0) {
        lines.push('    pass');
      } else {
        for (const f of st.fields) {
          lines.push(`    ${f.name}: ${rustTypeToPy(f.type, prefix, knownTypes)}`);
        }
      }
      lines.push('');
    }

    if (contract.errors.length > 0) {
      lines.push(`class ${prefix}Error(IntEnum):`);
      lines.push(`    """Error codes for ${contract.contractName}."""`);
      for (const err of contract.errors) {
        lines.push(`    ${err.name} = ${err.code}`);
      }
      lines.push('');
    }

    for (const fn of contract.methods) {
      const fnPascal = toPascalCase(fn.name);
      lines.push('@dataclass(frozen=True)');
      lines.push(`class ${prefix}${fnPascal}Input:`);
      lines.push(`    """${fn.doc.replace(/"/g, '\\"')}"""`);
      if (fn.inputs.length === 0) {
        lines.push('    pass');
      } else {
        for (const inp of fn.inputs) {
          lines.push(`    ${inp.name}: ${rustTypeToPy(inp.type, prefix, knownTypes)}`);
        }
      }
      lines.push('');
    }
  }

  return lines.join('\n');
}

// ── 4. Go Structs Generator ─────────────────────────────────────────────────

export function generateGoStructs(abi) {
  const lines = [
    '// Code generated by docs/scripts/generate-sdk-examples.mjs from Soroban contract ABIs (#451). DO NOT EDIT.',
    'package generated_test',
    '',
  ];

  for (const contract of abi.contracts) {
    const prefix = contract.pascalPrefix || toPascalCase(contract.contractId);
    const knownTypes = new Set([
      ...contract.structs.map((s) => s.name),
      ...contract.enums.map((e) => e.name),
    ]);

    for (const line of contract.doc.split('\n')) {
      lines.push(`// ${line}`);
    }
    lines.push(`const ${prefix}ContractDoc = ${JSON.stringify(contract.doc)}`);
    lines.push('');

    for (const en of contract.enums) {
      lines.push(`// ${prefix}${en.name}: ${en.doc}`);
      lines.push(`type ${prefix}${en.name} string`);
      lines.push('');
      if (en.variants.length > 0) {
        lines.push('const (');
        for (const v of en.variants) {
          lines.push(`\t${prefix}${en.name}${v.name} ${prefix}${en.name} = ${JSON.stringify(v.name)}`);
        }
        lines.push(')');
        lines.push('');
      }
    }

    for (const st of contract.structs) {
      lines.push(`// ${prefix}${st.name}: ${st.doc}`);
      lines.push(`type ${prefix}${st.name} struct {`);
      for (const f of st.fields) {
        const goField = toPascalCase(f.name);
        const goType = rustTypeToGo(f.type, prefix, knownTypes);
        lines.push(`\t${goField} ${goType} \`json:"${f.name}"\``);
      }
      lines.push('}');
      lines.push('');
    }

    if (contract.errors.length > 0) {
      lines.push(`// ${prefix}ErrorCode represents error codes for ${contract.contractName}.`);
      lines.push(`type ${prefix}ErrorCode uint32`);
      lines.push('');
      lines.push('const (');
      for (const err of contract.errors) {
        lines.push(`\t// ${prefix}Error${err.name}: ${err.doc}`);
        lines.push(`\t${prefix}Error${err.name} ${prefix}ErrorCode = ${err.code}`);
      }
      lines.push(')');
      lines.push('');
    }

    for (const fn of contract.methods) {
      const fnPascal = toPascalCase(fn.name);
      lines.push(`// ${prefix}${fnPascal}Input: ${fn.doc}`);
      lines.push(`type ${prefix}${fnPascal}Input struct {`);
      for (const inp of fn.inputs) {
        const goField = toPascalCase(inp.name);
        const goType = rustTypeToGo(inp.type, prefix, knownTypes);
        lines.push(`\t${goField} ${goType} \`json:"${inp.name}"\``);
      }
      lines.push('}');
      lines.push('');
      const outGoType = rustTypeToGo(fn.output, prefix, knownTypes);
      lines.push(`// ${prefix}${fnPascal}Output wraps the return value of ${contract.contractId}::${fn.name}.`);
      lines.push(`type ${prefix}${fnPascal}Output struct {`);
      lines.push(`\tResult ${outGoType} \`json:"result"\``);
      lines.push('}');
      lines.push('');
    }
  }

  return lines.join('\n');
}

/**
 * Generates all ABI and SDK type artifacts in memory:
 *   - `sdk/tests/generated/contracts_abi.json`
 *   - `sdk/tests/generated/typescript/contract_types.ts`
 *   - `sdk/tests/generated/python/contract_types.py`
 *   - `sdk/tests/generated/go/contract_types.go`
 *   - `contracts/common/src/abi.rs`
 */
export function buildAllAbiArtifacts(repoRoot, options = {}) {
  const abi = extractWorkspaceAbi(repoRoot, options);
  const sdkTestsDir = options.sdkTestsDir || path.join(repoRoot, 'sdk', 'tests', 'generated');
  const rustAbiPath = options.rustAbiPath || path.join(repoRoot, 'contracts', 'common', 'src', 'abi.rs');

  const files = new Map([
    [path.join(sdkTestsDir, 'contracts_abi.json'), `${JSON.stringify(abi, null, 2)}\n`],
    [path.join(sdkTestsDir, 'typescript', 'contract_types.ts'), generateTypeScriptTypes(abi)],
    [path.join(sdkTestsDir, 'python', 'contract_types.py'), generatePythonDataclasses(abi)],
    [path.join(sdkTestsDir, 'go', 'contract_types.go'), generateGoStructs(abi)],
    [rustAbiPath, generateRustBindings(abi)],
  ]);

  return { abi, files };
}

/**
 * Writes or checks (`checkOnly: true`) a map of generated file paths -> contents.
 * Normalizes CRLF/LF on comparison so cross-platform checkouts never false-fail.
 */
export function syncGeneratedFiles(filesMap, { checkOnly = false, repoRoot = process.cwd() } = {}) {
  const drifted = [];
  const written = [];

  for (const [filePath, expectedContent] of filesMap.entries()) {
    const normalizedExpected = expectedContent.replace(/\r\n/g, '\n');
    const exists = existsSync(filePath);
    const currentContent = exists ? readFileSync(filePath, 'utf8').replace(/\r\n/g, '\n') : null;

    if (currentContent !== normalizedExpected) {
      const rel = path.relative(repoRoot, filePath).replace(/\\/g, '/');
      if (checkOnly) {
        drifted.push(rel);
      } else {
        const dir = path.dirname(filePath);
        if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
        writeFileSync(filePath, normalizedExpected, 'utf8');
        written.push(rel);
      }
    }
  }

  return {
    inSync: drifted.length === 0,
    drifted,
    written,
    driftedFiles: drifted,
    updatedFiles: written,
  };
}
