"use client";

import { useEffect, useRef, useState } from "react";

export interface Dimensions {
  width: number;
  height: number;
}

/**
 * Hook for responsive chart re-rendering using a debounced ResizeObserver.
 *
 * @param delay Debounce delay in milliseconds (default: 150ms).
 */
export function useDebouncedResizeObserver<T extends HTMLElement = HTMLDivElement>(
  delay = 150
) {
  const ref = useRef<T | null>(null);
  const [dimensions, setDimensions] = useState<Dimensions>({ width: 0, height: 0 });
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    const element = ref.current;
    if (!element || typeof ResizeObserver === "undefined") {
      return;
    }

    const observer = new ResizeObserver((entries) => {
      if (!entries || entries.length === 0) return;
      const entry = entries[0];
      const { width, height } = entry.contentRect;

      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }

      timeoutRef.current = setTimeout(() => {
        setDimensions({
          width: Math.round(width),
          height: Math.round(height),
        });
      }, delay);
    });

    observer.observe(element);

    return () => {
      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }
      observer.disconnect();
    };
  }, [delay]);

  return { ref, width: dimensions.width, height: dimensions.height };
}
