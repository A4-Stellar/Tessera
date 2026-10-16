import { readFileSync, readdirSync } from 'node:fs';
import { dirname, relative, resolve } from 'node:path';
import ts from 'typescript';

const docsDir = resolve(process.cwd());
const sourceExtensions = new Set(['ts', 'tsx', 'typescript']);

function getMdxFiles(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.name === 'node_modules' || entry.name === '.next') continue;
    const filePath = resolve(directory, entry.name);
    if (entry.isDirectory()) files.push(...getMdxFiles(filePath));
    else if (entry.isFile() && filePath.endsWith('.mdx')) files.push(filePath);
  }
  return files;
}

function extractSamples(filePath, source) {
  const samples = [];
  const lines = source.split(/\r?\n/);
  let sample;

  for (let index = 0; index < lines.length; index += 1) {
    const opening = lines[index].match(/^\s{0,3}```\s*([^\s`]+)?(?:\s+.*)?$/);
    if (opening && sourceExtensions.has((opening[1] ?? '').toLowerCase())) {
      sample = {
        filePath,
        language: opening[1].toLowerCase() === 'typescript' ? 'ts' : opening[1].toLowerCase(),
        startLine: index + 1,
        lines: [],
      };
      continue;
    }

    if (sample && /^\s{0,3}```\s*$/.test(lines[index])) {
      samples.push({ ...sample, code: sample.lines.join('\n') });
      sample = undefined;
      continue;
    }

    if (sample) sample.lines.push(lines[index]);
  }

  return samples;
}

function formatDiagnostic(diagnostic, samplesByFile) {
  const message = ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n');
  if (!diagnostic.file) return `  ${message}`;

  const sample = samplesByFile.get(diagnostic.file.fileName);
  const position = diagnostic.file.getLineAndCharacterOfPosition(diagnostic.start ?? 0);
  if (!sample) {
    return `  ${diagnostic.file.fileName}:${position.line + 1}:${position.character + 1} ${message}`;
  }

  const fileName = relative(docsDir, sample.filePath);
  return `  ${fileName}:${sample.startLine + position.line + 1}:${position.character + 1} ${message}`;
}

const configPath = resolve(docsDir, 'tsconfig.json');
const config = ts.readConfigFile(configPath, ts.sys.readFile);
if (config.error) {
  console.error(ts.formatDiagnosticsWithColorAndContext([config.error], {
    getCanonicalFileName: (fileName) => fileName,
    getCurrentDirectory: () => docsDir,
    getNewLine: () => '\n',
  }));
  process.exit(1);
}

const parsedConfig = ts.parseJsonConfigFileContent(config.config, ts.sys, docsDir);
const samples = [];
for (const filePath of getMdxFiles(docsDir)) {
  samples.push(...extractSamples(filePath, readFileSync(filePath, 'utf8')));
}

if (samples.length === 0) {
  console.log('No TypeScript or TSX code samples found.');
  process.exit(0);
}

const virtualSources = new Map();
const samplesByFile = new Map();
for (const [index, sample] of samples.entries()) {
  const virtualFileName = resolve(
    dirname(sample.filePath),
    `.mdx-sample-${index}.${sample.language}`,
  );
  const code = `${sample.code}\nexport {};\n`;
  virtualSources.set(virtualFileName, code);
  samplesByFile.set(virtualFileName, sample);
  sample.virtualFileName = virtualFileName;
}

const options = {
  ...parsedConfig.options,
  strict: true,
  noEmit: true,
  noEmitOnError: true,
  incremental: false,
};
const defaultHost = ts.createCompilerHost(options);
const host = {
  ...defaultHost,
  getSourceFile(fileName, languageVersion, onError, shouldCreateNewSourceFile) {
    const source = virtualSources.get(resolve(fileName));
    if (source !== undefined) {
      return ts.createSourceFile(fileName, source, languageVersion, true);
    }
    return defaultHost.getSourceFile(fileName, languageVersion, onError, shouldCreateNewSourceFile);
  },
  readFile(fileName) {
    return virtualSources.get(resolve(fileName)) ?? defaultHost.readFile(fileName);
  },
  fileExists(fileName) {
    return virtualSources.has(resolve(fileName)) || defaultHost.fileExists(fileName);
  },
};

const program = ts.createProgram(
  samples.map((sample) => sample.virtualFileName),
  options,
  host,
);
const diagnostics = ts.getPreEmitDiagnostics(program).filter((diagnostic) => {
  // Existing documentation includes intentionally excerpted snippets whose
  // surrounding declarations are omitted. Keep unresolved local names from
  // those excerpts out while still reporting broken imports and type errors.
  if (diagnostic.code === 2304) return false;
  return !diagnostic.file || samplesByFile.has(resolve(diagnostic.file.fileName));
});

if (diagnostics.length > 0) {
  console.error(`Found ${diagnostics.length} TypeScript error${diagnostics.length === 1 ? '' : 's'} in MDX samples:`);
  for (const diagnostic of diagnostics) {
    console.error(formatDiagnostic(diagnostic, samplesByFile));
  }
  process.exit(1);
}

console.log(`Type-checked ${samples.length} TypeScript/TSX MDX sample${samples.length === 1 ? '' : 's'}.`);
