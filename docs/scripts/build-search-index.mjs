import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const docsDir = path.join(__dirname, '../app/docs');
const publicDir = path.join(__dirname, '../public');

function getMdxFiles(dir, fileList = []) {
  const files = fs.readdirSync(dir);
  for (const file of files) {
    const stat = fs.statSync(path.join(dir, file));
    if (stat.isDirectory()) {
      getMdxFiles(path.join(dir, file), fileList);
    } else if (file.endsWith('.mdx')) {
      fileList.push(path.join(dir, file));
    }
  }
  return fileList;
}

const files = getMdxFiles(docsDir);

const documents = files.map((file, id) => {
  const content = fs.readFileSync(file, 'utf-8');
  let title = 'Untitled';
  const titleMatch = content.match(/title:\s*["']([^"']+)["']/);
  if (titleMatch) {
    title = titleMatch[1];
  }
  
  // Remove imports and exports
  let cleanContent = content.replace(/export\s+const\s+metadata\s*=\s*\{[\s\S]*?\};/, '');
  cleanContent = cleanContent.replace(/import\s+[^;]+;/g, '');
  // Extract headers
  const headers = [];
  const headerMatches = cleanContent.matchAll(/^#+\s+(.+)$/gm);
  for (const match of headerMatches) {
    headers.push(match[1]);
  }

  const relativePath = path.relative(docsDir, file);
  let route = '/docs/' + relativePath.replace(/\\/g, '/').replace(/\/page\.mdx$/, '').replace(/\.mdx$/, '');
  if (route.endsWith('/page')) route = route.replace(/\/page$/, '');

  return {
    id,
    route,
    title,
    headers: headers.join(' '),
    content: cleanContent
  };
});

if (!fs.existsSync(publicDir)) {
  fs.mkdirSync(publicDir, { recursive: true });
}

fs.writeFileSync(path.join(publicDir, 'search-index.json'), JSON.stringify(documents));
console.log('Search index generated with ' + documents.length + ' documents.');
