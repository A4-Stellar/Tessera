import { readFileSync, writeFileSync, readdirSync, statSync, unlinkSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { execSync } from 'node:child_process';
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
      const tempFile = join(tmpdir(), `mdx-check-${Date.now()}-${Math.floor(Math.random() * 1000)}.${lang}`);
      writeFileSync(tempFile, node.value);
      
      const program = ts.createProgram([tempFile], {
        noEmit: true,
        esModuleInterop: true,
        skipLibCheck: true,
        jsx: ts.JsxEmit.React
      });
      const emitResult = program.emit();
      const allDiagnostics = ts.getPreEmitDiagnostics(program).concat(emitResult.diagnostics);
      
      let snippetHasError = false;
      for (const diag of allDiagnostics) {
        if (diag.category === ts.DiagnosticCategory.Error) {
          snippetHasError = true;
          const message = ts.flattenDiagnosticMessageText(diag.messageText, '\n');
          console.error(`[TS Error] in ${file}:\n${message}`);
        }
      }
      if (snippetHasError) {
        hasErrors = true;
      }
      try {
        unlinkSync(tempFile);
      } catch (e) {}
      
    } else if (lang === 'rust' || lang === 'rs') {
      const tempFile = join(tmpdir(), `mdx-check-${Date.now()}-${Math.floor(Math.random() * 1000)}.rs`);
      writeFileSync(tempFile, node.value);
      try {
        execSync(`rustc --edition=2021 --crate-type=lib -Z parse-only ${tempFile}`, { stdio: 'pipe' });
      } catch (err) {
        hasErrors = true;
        console.error(`[Rust Error] in ${file}:\n${err.stderr ? err.stderr.toString() : err.message}`);
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
  console.log("All code samples are valid.");
}
