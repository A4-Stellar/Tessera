import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - api-rate-limits', () => {
  it('executes TypeScript example from api-rate-limits block 1', async () => {
    // Source: docs/content/en/docs/api-rate-limits.mdx:98
    const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
    
    const etagCache = new Map<string, { etag: string; data: unknown }>();
    
    export async function fetchWithEtag<T>(path: string): Promise<T> {
      const url = `${API}${path}`;
      const cached = etagCache.get(url);
    
      const headers: HeadersInit = cached
        ? { "If-None-Match": cached.etag }
        : {};
    
      const res = await fetch(url, { headers });
    
      if (res.status === 304 && cached) {
        // Server confirmed nothing changed — reuse the cached body.
        return cached.data as T;
      }
    
      if (!res.ok) throw new Error(`API ${res.status}`);
    
      const data = (await res.json()) as T;
      const etag = res.headers.get("ETag");
      if (etag) etagCache.set(url, { etag, data });
    
      return data;
    }
  });
});
