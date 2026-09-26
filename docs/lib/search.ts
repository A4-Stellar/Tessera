import FlexSearch from 'flexsearch';

export type SearchDocument = {
  id: number;
  route: string;
  title: string;
  headers: string;
  content: string;
};

let index: any = null;
let documents: SearchDocument[] = [];

export async function initSearch() {
  if (index) return;
  
  try {
    const res = await fetch('/search-index.json');
    documents = await res.json();

    index = new FlexSearch.Document({
      document: {
        id: "id",
        index: ["title", "headers", "content"],
        store: true
      },
      tokenize: "forward"
    });

    for (const doc of documents) {
      index.add(doc);
    }
  } catch (error) {
    console.error("Failed to initialize search", error);
  }
}

export function search(query: string): SearchDocument[] {
  if (!index || !query) return [];
  
  const results = index.search(query, {
    enrich: true,
    limit: 5
  });
  
  const uniqueResults = new Map<number, SearchDocument>();
  
  for (const fieldResult of results) {
    for (const res of fieldResult.result) {
      if (!uniqueResults.has(res.id)) {
        uniqueResults.set(res.id, res.doc);
      }
    }
  }
  
  return Array.from(uniqueResults.values());
}
