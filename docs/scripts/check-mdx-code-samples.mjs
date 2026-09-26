import { readFileSync, writeFileSync, readdirSync, statSync, unlinkSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import ts from 'typescript';
import { remark } from 'remark';
import { visit } from 'unist-util-visit';

const docsDir = resolve(process.cwd());

function getMdxFiles(dir) {
  let results = [];
  const list = readdirSync(dir);
  for (const file of list) {
    const filePath = join(dir, file);
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

function addSemicolonsToRustDeclarations(code) {
  const declarationStarts = [];
  const declarationPattern = /(?:^|\n)[ \t]*(?:(?:pub(?:\([^)]*\))?|async)\s+)*fn\s+[A-Za-z_]\w*/g;
  for (const match of code.matchAll(declarationPattern)) {
    declarationStarts.push(match.index + match[0].lastIndexOf('fn'));
  }

  const insertions = [];
  for (const start of declarationStarts) {
    const openParen = code.indexOf('(', start);
    if (openParen < 0) continue;
    let depth = 0;
    let closeParen = -1;
    for (let i = openParen; i < code.length; i += 1) {
      if (code[i] === '(') depth += 1;
      if (code[i] === ')') {
        depth -= 1;
        if (depth === 0) {
          closeParen = i;
          break;
        }
      }
    }
    if (closeParen < 0) continue;

    const nextFunction = declarationStarts.find((position) => position > closeParen) ?? code.length;
    const nextBrace = code.indexOf('{', closeParen + 1);
    const nextSemicolon = code.indexOf(';', closeParen + 1);
    const nextCloseBrace = code.indexOf('}', closeParen + 1);
    const boundary = Math.min(
      ...[nextFunction, nextBrace, nextSemicolon, nextCloseBrace, code.length].filter((position) => position >= 0),
    );
    if (boundary === nextBrace || boundary === nextSemicolon) continue;

    let end = boundary - 1;
    while (end > closeParen && /\s/.test(code[end])) end -= 1;
    if (end >= closeParen && code[end] !== ';') insertions.push(end + 1);
  }

  for (const position of insertions.sort((a, b) => b - a)) {
    code = `${code.slice(0, position)};${code.slice(position)}`;
  }
  return code;
}

const files = getMdxFiles(docsDir);

let hasErrors = false;

for (const file of files) {
  const content = readFileSync(file, 'utf-8');
  let tree;
  try {
    tree = remark().parse(content);
  } catch (err) {
    console.error(`Failed to parse ${file}: ${err.message}`);
    continue;
  }
  
  visit(tree, 'code', (node) => {
    const lang = node.lang ? node.lang.toLowerCase() : null;
    if (lang === 'ts' || lang === 'tsx') {
      const sourceFile = ts.createSourceFile(
        `sample.${lang}`,
        node.value,
        ts.ScriptTarget.Latest,
        true,
        lang === 'tsx' ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
      );
      for (const diag of sourceFile.parseDiagnostics) {
        hasErrors = true;
        const message = ts.flattenDiagnosticMessageText(diag.messageText, '\n');
        console.error(`[TS Syntax Error] in ${file}:\n${message}`);
      }

    } else if (lang === 'rust' || lang === 'rs') {
      const tempFile = join(tmpdir(), `mdx-check-${Date.now()}-${Math.floor(Math.random() * 1000)}.rs`);
      writeFileSync(tempFile, node.value);
      try {
        execFileSync('rustfmt', ['--edition=2021', '--emit=stdout', tempFile], { stdio: 'pipe' });
      } catch (err) {
        try {
          // API signatures in the docs are intentionally bodyless; a trait accepts those declarations.
          writeFileSync(tempFile, `trait __MdxSample {\n${addSemicolonsToRustDeclarations(node.value)}\n}`);
          execFileSync('rustfmt', ['--edition=2021', '--emit=stdout', tempFile], { stdio: 'pipe' });
        } catch (wrappedErr) {
          hasErrors = true;
          console.error(`[Rust Syntax Error] in ${file}:\n${wrappedErr.stderr ? wrappedErr.stderr.toString() : wrappedErr.message}`);
        }
      }
      try {
        unlinkSync(tempFile);
      } catch (e) {}
    }
  });
}

if (hasErrors) {
  console.error("Code sample verification failed.");
  process.exit(1);
} else {
  console.log("All code samples are syntactically valid.");
}
