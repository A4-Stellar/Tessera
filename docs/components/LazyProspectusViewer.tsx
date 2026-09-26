"use client";

import { lazy, Suspense, useEffect, useRef, useState } from "react";
import type { ProspectusViewerProps } from "./ProspectusViewer";

const ProspectusViewer = lazy(() =>
  import("./ProspectusViewer").then((module) => ({ default: module.ProspectusViewer }))
);

/**
 * Issue #62: defers both the viewer code and the PDF download until the
 * placeholder scrolls near the viewport (falls back to immediate mount when
 * IntersectionObserver is unavailable).
 */
export function LazyProspectusViewer(props: ProspectusViewerProps & { rootMargin?: string }) {
  const { rootMargin = "200px", ...viewerProps } = props;
  const holderRef = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const element = holderRef.current;
    if (!element) return;
    if (typeof IntersectionObserver === "undefined") {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setVisible(true);
          observer.disconnect();
        }
      },
      { rootMargin }
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [rootMargin]);

  return (
    <div ref={holderRef} data-testid="lazy-prospectus">
      {visible ? (
        <Suspense fallback={<p role="status">Loading prospectus viewer…</p>}>
          <ProspectusViewer {...viewerProps} />
        </Suspense>
      ) : (
        <p className="text-sm text-base-300">{viewerProps.title ?? "Prospectus"} loads when scrolled into view.</p>
      )}
    </div>
  );
}

export default LazyProspectusViewer;
