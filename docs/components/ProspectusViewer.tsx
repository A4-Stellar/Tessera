"use client";

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type {
  PDFDocumentLoadingTask,
  PDFDocumentProxy,
  PDFPageProxy,
  RenderTask,
} from "pdfjs-dist";

export interface ProspectusViewerProps {
  src?: string;
  pdfUrl?: string;
  url?: string;
  expectedHash?: string;
  expectedSha256?: string;
  expectedOnChainHash?: string;
  title?: string;
  className?: string;
  initialZoom?: number;
  workerSrc?: string;
}

type ViewerStatus = "idle" | "loading" | "ready" | "error";
type VerificationStatus =
  | "not-requested"
  | "verifying"
  | "verified"
  | "mismatch"
  | "unavailable"
  | "invalid";

type SearchMatch = {
  pageNumber: number;
  occurrence: number;
};

const PDF_WORKER_VERSION = "4.10.38";
const MIN_ZOOM = 0.5;
const MAX_ZOOM = 3;
const MAX_SEARCH_RESULTS = 1000;

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(Math.max(value, minimum), maximum);
}

export function normalizeSha256(value: string): string | null {
  const normalized = value.trim().replace(/^sha-?256:/i, "").replace(/^0x/i, "").toLowerCase();
  return /^[0-9a-f]{64}$/.test(normalized) ? normalized : null;
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");
}

export async function sha256Hex(data: ArrayBuffer | Uint8Array): Promise<string> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) {
    throw new Error("SHA-256 verification is not available in this browser.");
  }
  const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);
  const buffer = bytes.slice().buffer as ArrayBuffer;
  const digest = await subtle.digest("SHA-256", buffer);
  return bytesToHex(new Uint8Array(digest));
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error.trim()) return error;
  return fallback;
}

function isAbortError(error: unknown): boolean {
  return typeof DOMException !== "undefined" && error instanceof DOMException && error.name === "AbortError";
}

export function isSafePdfSource(value: string): boolean {
  if (!value.trim()) return false;
  try {
    const base = typeof window !== "undefined" ? window.location.origin : "https://docs.invalid";
    const endpoint = new URL(value, base);
    if (endpoint.username || endpoint.password) return false;
    if (endpoint.protocol === "https:") return true;
    return (
      endpoint.protocol === "http:" &&
      (endpoint.hostname === "localhost" || endpoint.hostname === "127.0.0.1" || endpoint.hostname === "[::1]")
    );
  } catch {
    return false;
  }
}

function textFromItems(items: unknown): string {
  if (!Array.isArray(items)) return "";
  return items
    .map((item) => {
      if (typeof item !== "object" || item === null || !("str" in item)) return "";
      const value = (item as { str?: unknown }).str;
      return typeof value === "string" ? value : "";
    })
    .join(" ");
}

function defaultWorkerSource(version: string | undefined): string {
  const resolvedVersion = version && /^\d+\.\d+\.\d+$/.test(version) ? version : PDF_WORKER_VERSION;
  return `https://cdnjs.cloudflare.com/ajax/libs/pdf.js/${resolvedVersion}/pdf.worker.min.mjs`;
}

export function ProspectusViewer({
  src,
  pdfUrl,
  url,
  expectedHash,
  expectedSha256,
  expectedOnChainHash,
  title = "Prospectus",
  className = "",
  initialZoom = 1,
  workerSrc,
}: ProspectusViewerProps) {
  const titleId = useId();
  const searchId = useId();
  const pageInputId = useId();
  const source = src ?? pdfUrl ?? url ?? "";
  const sourceIsSafe = isSafePdfSource(source);
  const expectedValue = expectedHash ?? expectedSha256 ?? expectedOnChainHash ?? "";
  const normalizedExpectedHash = useMemo(
    () => (expectedValue ? normalizeSha256(expectedValue) : null),
    [expectedValue]
  );
  const [status, setStatus] = useState<ViewerStatus>("idle");
  const [error, setError] = useState<string | null>(null);
  const [renderError, setRenderError] = useState<string | null>(null);
  const [verification, setVerification] = useState<VerificationStatus>("not-requested");
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const [pageCount, setPageCount] = useState(0);
  const [pageNumber, setPageNumber] = useState(1);
  const [zoom, setZoom] = useState(() => clamp(initialZoom, MIN_ZOOM, MAX_ZOOM));
  const [searchTerm, setSearchTerm] = useState("");
  const [searchMatches, setSearchMatches] = useState<SearchMatch[]>([]);
  const [activeMatch, setActiveMatch] = useState(0);
  const [searchMessage, setSearchMessage] = useState("");
  const [searching, setSearching] = useState(false);
  const [viewportWidth, setViewportWidth] = useState(0);
  const viewportRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const documentRef = useRef<PDFDocumentProxy | null>(null);
  const loadingTaskRef = useRef<PDFDocumentLoadingTask | null>(null);
  const renderTaskRef = useRef<RenderTask | null>(null);
  const touchRef = useRef<{
    startX: number;
    startY: number;
    lastDistance: number | null;
  } | null>(null);

  const goToPage = useCallback(
    (requestedPage: number) => {
      setPageNumber((currentPage) => {
        const lastPage = pageCount || 1;
        return clamp(Math.round(requestedPage), 1, lastPage);
      });
    },
    [pageCount]
  );

  useEffect(() => {
    let cancelled = false;
    const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
    const previousDocument = documentRef.current;
    const previousTask = loadingTaskRef.current;
    documentRef.current = null;
    loadingTaskRef.current = null;
    if (previousTask) void previousTask.destroy();
    if (previousDocument) void previousDocument.destroy();
    setPdf(null);
    setPageCount(0);
    setPageNumber(1);
    setSearchMatches([]);
    setActiveMatch(0);
    setSearchMessage("");
    setRenderError(null);
    setError(null);
    setVerification(expectedValue ? "verifying" : "not-requested");
    setStatus("loading");

    async function loadDocument() {
      if (!source) {
        throw new Error("A PDF source is required.");
      }
      if (!isSafePdfSource(source)) {
        throw new Error("The prospectus source must use HTTPS or a local development origin.");
      }
      if (workerSrc?.trim() && !isSafePdfSource(workerSrc)) {
        throw new Error("The PDF worker must use HTTPS or a local development origin.");
      }
      const response = await fetch(source, {
        credentials: "omit",
        cache: "no-store",
        signal: controller?.signal,
      });
      if (response.ok === false) {
        throw new Error(`The prospectus could not be loaded (${response.status}).`);
      }
      if (!response.arrayBuffer) {
        throw new Error("The prospectus response did not contain PDF data.");
      }
      const bytes = new Uint8Array(await response.arrayBuffer());
      if (bytes.byteLength === 0) {
        throw new Error("The prospectus file is empty.");
      }

      if (expectedValue) {
        if (!normalizedExpectedHash) {
          setVerification("invalid");
        } else {
          try {
            const actualHash = await sha256Hex(bytes);
            if (cancelled) return;
            setVerification(actualHash === normalizedExpectedHash ? "verified" : "mismatch");
          } catch (hashError) {
            if (cancelled) return;
            setVerification("unavailable");
            setError(errorMessage(hashError, "The prospectus hash could not be verified."));
          }
        }
      }

      const pdfjs = await import("pdfjs-dist");
      if (cancelled) return;
      pdfjs.GlobalWorkerOptions.workerSrc = workerSrc?.trim() || defaultWorkerSource(pdfjs.version);
      const loadingTask = pdfjs.getDocument({
        data: bytes,
        isEvalSupported: false,
      });
      loadingTaskRef.current = loadingTask;
      const loadedDocument = await loadingTask.promise;
      if (cancelled) {
        await loadingTask.destroy();
        return;
      }
      documentRef.current = loadedDocument;
      setPdf(loadedDocument);
      setPageCount(loadedDocument.numPages);
      setPageNumber((currentPage) => clamp(currentPage, 1, loadedDocument.numPages || 1));
      setStatus("ready");
    }

    void loadDocument().catch((loadError: unknown) => {
      if (cancelled || isAbortError(loadError)) return;
      setStatus("error");
      setError(errorMessage(loadError, "The prospectus could not be loaded."));
    });

    return () => {
      cancelled = true;
      controller?.abort();
      const task = loadingTaskRef.current;
      const loadedDocument = documentRef.current;
      loadingTaskRef.current = null;
      documentRef.current = null;
      if (task) void task.destroy();
      if (loadedDocument) void loadedDocument.destroy();
    };
  }, [expectedValue, normalizedExpectedHash, source, workerSrc]);

  useEffect(() => {
    if (!pdf || !viewportRef.current) return;
    const element = viewportRef.current;
    const updateWidth = () => setViewportWidth(element.clientWidth);
    updateWidth();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", updateWidth);
      return () => window.removeEventListener("resize", updateWidth);
    }
    const observer = new ResizeObserver(updateWidth);
    observer.observe(element);
    return () => observer.disconnect();
  }, [pdf]);

  useEffect(() => {
    if (!pdf || !canvasRef.current) return;
    let cancelled = false;
    const canvas = canvasRef.current;
    const loadedPdf = pdf;

    async function renderPage() {
      setRenderError(null);
      try {
        const page: PDFPageProxy = await loadedPdf.getPage(pageNumber);
        if (cancelled) return;
        const unscaledViewport = page.getViewport({ scale: 1 });
        const availableWidth = Math.max((viewportWidth || viewportRef.current?.clientWidth || 640) - 32, 280);
        const fitScale = Math.min(availableWidth / unscaledViewport.width, 2);
        const pageViewport = page.getViewport({ scale: fitScale * zoom });
        const devicePixelRatio = Math.min(window.devicePixelRatio || 1, 2);
        const context = canvas.getContext("2d");
        if (!context) {
          throw new Error("This browser could not create a PDF canvas.");
        }
        canvas.width = Math.max(1, Math.floor(pageViewport.width * devicePixelRatio));
        canvas.height = Math.max(1, Math.floor(pageViewport.height * devicePixelRatio));
        canvas.style.width = `${Math.max(1, Math.floor(pageViewport.width))}px`;
        canvas.style.height = `${Math.max(1, Math.floor(pageViewport.height))}px`;
        const renderTask = page.render({
          canvasContext: context,
          viewport: pageViewport,
          transform: devicePixelRatio === 1 ? undefined : [devicePixelRatio, 0, 0, devicePixelRatio, 0, 0],
        });
        renderTaskRef.current = renderTask;
        await renderTask.promise;
        if (!cancelled) renderTaskRef.current = null;
      } catch (renderFailure: unknown) {
        if (!cancelled) {
          setRenderError(errorMessage(renderFailure, "The PDF page could not be rendered."));
        }
      }
    }

    void renderPage();
    return () => {
      cancelled = true;
      renderTaskRef.current?.cancel();
      renderTaskRef.current = null;
    };
  }, [pageNumber, pdf, viewportWidth, zoom]);

  useEffect(() => {
    const element = viewportRef.current;
    if (!element || !pageCount) return;

    const distanceBetween = (touchList: TouchList): number | null => {
      if (touchList.length < 2) return null;
      const first = touchList[0];
      const second = touchList[1];
      return Math.hypot(second.clientX - first.clientX, second.clientY - first.clientY);
    };

    const handleTouchStart = (event: TouchEvent) => {
      const distance = distanceBetween(event.touches);
      if (distance !== null) {
        touchRef.current = { startX: 0, startY: 0, lastDistance: distance };
        return;
      }
      const touch = event.touches[0];
      if (touch) touchRef.current = { startX: touch.clientX, startY: touch.clientY, lastDistance: null };
    };

    const handleTouchMove = (event: TouchEvent) => {
      const distance = distanceBetween(event.touches);
      const currentTouch = touchRef.current;
      if (distance === null || !currentTouch) return;
      const previousDistance = currentTouch.lastDistance;
      if (previousDistance === null) return;
      if (previousDistance > 0) {
        event.preventDefault();
        setZoom((currentZoom) => clamp(currentZoom * (distance / previousDistance), MIN_ZOOM, MAX_ZOOM));
      }
      currentTouch.lastDistance = distance;
    };

    const handleTouchEnd = (event: TouchEvent) => {
      const currentTouch = touchRef.current;
      if (!currentTouch || currentTouch.lastDistance !== null) {
        touchRef.current = null;
        return;
      }
      const touch = event.changedTouches[0];
      if (touch) {
        const deltaX = touch.clientX - currentTouch.startX;
        const deltaY = touch.clientY - currentTouch.startY;
        if (Math.abs(deltaX) > 50 && Math.abs(deltaX) > Math.abs(deltaY)) {
          goToPage(pageNumber + (deltaX < 0 ? 1 : -1));
        }
      }
      touchRef.current = null;
    };

    element.addEventListener("touchstart", handleTouchStart, { passive: true });
    element.addEventListener("touchmove", handleTouchMove, { passive: false });
    element.addEventListener("touchend", handleTouchEnd, { passive: true });
    element.addEventListener("touchcancel", handleTouchEnd, { passive: true });
    return () => {
      element.removeEventListener("touchstart", handleTouchStart);
      element.removeEventListener("touchmove", handleTouchMove);
      element.removeEventListener("touchend", handleTouchEnd);
      element.removeEventListener("touchcancel", handleTouchEnd);
    };
  }, [goToPage, pageCount, pageNumber]);

  async function searchDocument(event?: React.FormEvent<HTMLFormElement>) {
    event?.preventDefault();
    const query = searchTerm.trim();
    setSearchMessage("");
    setSearchMatches([]);
    setActiveMatch(0);
    if (!query || !pdf) {
      setSearchMessage(query ? "The document is not ready yet." : "Enter text to search.");
      return;
    }
    setSearching(true);
    try {
      const matches: SearchMatch[] = [];
      for (let currentPage = 1; currentPage <= pageCount; currentPage += 1) {
        const page = await pdf.getPage(currentPage);
        const textContent = await page.getTextContent();
        const text = textFromItems(textContent.items).toLocaleLowerCase();
        const needle = query.toLocaleLowerCase();
        let offset = text.indexOf(needle);
        let occurrence = 0;
        while (offset !== -1 && matches.length < MAX_SEARCH_RESULTS) {
          matches.push({ pageNumber: currentPage, occurrence });
          occurrence += 1;
          offset = text.indexOf(needle, offset + Math.max(needle.length, 1));
        }
        if (matches.length >= MAX_SEARCH_RESULTS) break;
      }
      setSearchMatches(matches);
      if (matches.length > 0) {
        setActiveMatch(0);
        goToPage(matches[0].pageNumber);
        setSearchMessage(`1 of ${matches.length} matches`);
      } else {
        setSearchMessage("No matches found");
      }
    } catch (searchFailure: unknown) {
      setSearchMessage(errorMessage(searchFailure, "The document text could not be searched."));
    } finally {
      setSearching(false);
    }
  }

  function showMatch(index: number) {
    if (searchMatches.length === 0) return;
    const nextIndex = (index + searchMatches.length) % searchMatches.length;
    setActiveMatch(nextIndex);
    goToPage(searchMatches[nextIndex].pageNumber);
    setSearchMessage(`${nextIndex + 1} of ${searchMatches.length} matches`);
  }

  function changeZoom(amount: number) {
    setZoom((currentZoom) => clamp(Number((currentZoom + amount).toFixed(2)), MIN_ZOOM, MAX_ZOOM));
  }

  const verificationLabel = useMemo(() => {
    switch (verification) {
      case "verifying":
        return "Verifying SHA-256 hash…";
      case "verified":
        return "SHA-256 verified against the expected on-chain hash.";
      case "mismatch":
        return "SHA-256 verification failed: the downloaded file does not match the expected on-chain hash.";
      case "unavailable":
        return "SHA-256 verification is unavailable in this browser; the file is not verified.";
      case "invalid":
        return "The expected on-chain hash is not a valid SHA-256 value; the file is not verified.";
      default:
        return "No expected on-chain hash was supplied.";
    }
  }, [verification]);

  return (
    <section
      className={`my-6 overflow-hidden rounded-xl border border-white/10 bg-white/[0.03] ${className}`}
      aria-labelledby={titleId}
      aria-busy={status === "loading"}
      data-testid="prospectus-viewer"
    >
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-white/10 px-4 py-3">
        <div>
          <h3 id={titleId} className="text-base font-bold text-base-50">
            {title}
          </h3>
          <p className="mt-1 text-xs text-base-300">PDF.js viewer with verified source integrity</p>
        </div>
        <a
          href={sourceIsSafe ? source : undefined}
          target="_blank"
          rel="noopener noreferrer"
          className="rounded-md border border-white/10 px-2.5 py-1.5 text-xs font-medium text-brand-300 hover:border-brand-500/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        >
          Open source PDF
        </a>
      </header>

      <div className="space-y-3 p-4">
        {expectedValue && (
          <div
            role={verification === "mismatch" || verification === "invalid" ? "alert" : "status"}
            className={`rounded-md border px-3 py-2 text-xs ${
              verification === "verified"
                ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-300"
                : verification === "mismatch" || verification === "invalid"
                ? "border-red-500/30 bg-red-500/10 text-red-300"
                : "border-gold-500/30 bg-gold-500/10 text-gold-300"
            }`}
          >
            {verificationLabel}
          </div>
        )}

        {error && (
          <div role="alert" className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {error}
            <a
              href={sourceIsSafe ? source : undefined}
              target="_blank"
              rel="noopener noreferrer"
              className="ml-2 font-semibold underline underline-offset-2"
            >
              Download the source instead
            </a>
          </div>
        )}

        {status === "loading" && (
          <div role="status" className="flex items-center gap-2 rounded-md border border-white/10 bg-base-900/60 px-3 py-3 text-sm text-base-200">
            <span className="h-4 w-4 animate-spin rounded-full border-2 border-brand-400 border-t-transparent" aria-hidden="true" />
            <span>Loading prospectus…</span>
          </div>
        )}

        {status === "error" && !error && (
          <p role="alert" className="text-sm text-red-300">
            The prospectus could not be displayed.
          </p>
        )}

        {status === "ready" && pdf && (
          <>
            <div className="flex flex-wrap items-center justify-between gap-3" role="toolbar" aria-label="Prospectus controls">
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => goToPage(pageNumber - 1)}
                  disabled={pageNumber <= 1}
                  className="rounded-md border border-white/10 px-2.5 py-1.5 text-sm text-base-100 hover:border-brand-500/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 disabled:cursor-not-allowed disabled:opacity-40"
                  aria-label="Previous page"
                >
                  Previous
                </button>
                <label htmlFor={pageInputId} className="sr-only">
                  Current PDF page
                </label>
                <input
                  id={pageInputId}
                  type="number"
                  min={1}
                  max={pageCount}
                  value={pageNumber}
                  onChange={(event) => {
                    const nextPage = Number(event.target.value);
                    if (Number.isFinite(nextPage)) goToPage(nextPage);
                  }}
                  className="w-16 rounded-md border border-white/10 bg-base-900 px-2 py-1.5 text-center text-sm text-base-100 focus:border-brand-500/60 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
                />
                <span className="text-sm text-base-300">of {pageCount}</span>
                <button
                  type="button"
                  onClick={() => goToPage(pageNumber + 1)}
                  disabled={pageNumber >= pageCount}
                  className="rounded-md border border-white/10 px-2.5 py-1.5 text-sm text-base-100 hover:border-brand-500/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 disabled:cursor-not-allowed disabled:opacity-40"
                  aria-label="Next page"
                >
                  Next
                </button>
              </div>

              <div className="flex items-center gap-2" aria-label="Zoom controls">
                <button
                  type="button"
                  onClick={() => changeZoom(-0.1)}
                  disabled={zoom <= MIN_ZOOM}
                  className="rounded-md border border-white/10 px-2.5 py-1.5 text-sm text-base-100 hover:border-brand-500/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 disabled:cursor-not-allowed disabled:opacity-40"
                  aria-label="Zoom out"
                >
                  −
                </button>
                <output className="min-w-12 text-center text-xs text-base-200" aria-label="Zoom level">
                  {Math.round(zoom * 100)}%
                </output>
                <button
                  type="button"
                  onClick={() => changeZoom(0.1)}
                  disabled={zoom >= MAX_ZOOM}
                  className="rounded-md border border-white/10 px-2.5 py-1.5 text-sm text-base-100 hover:border-brand-500/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 disabled:cursor-not-allowed disabled:opacity-40"
                  aria-label="Zoom in"
                >
                  +
                </button>
              </div>
            </div>

            <form onSubmit={searchDocument} className="flex flex-wrap items-end gap-2" role="search">
              <div className="min-w-0 flex-1">
                <label htmlFor={searchId} className="mb-1 block text-xs font-semibold text-base-300">
                  Search prospectus text
                </label>
                <input
                  id={searchId}
                  type="search"
                  value={searchTerm}
                  onChange={(event) => setSearchTerm(event.target.value)}
                  placeholder="Search extracted PDF text"
                  className="w-full rounded-md border border-white/10 bg-base-900 px-2.5 py-1.5 text-sm text-base-100 placeholder-base-400 focus:border-brand-500/60 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
                />
              </div>
              <button
                type="submit"
                disabled={searching}
                className="rounded-md border border-brand-500/40 px-3 py-1.5 text-sm font-semibold text-brand-300 hover:bg-brand-500/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 disabled:cursor-not-allowed disabled:opacity-50"
              >
                {searching ? "Searching…" : "Search"}
              </button>
              {searchMatches.length > 0 && (
                <div className="flex items-center gap-1">
                  <button
                    type="button"
                    onClick={() => showMatch(activeMatch - 1)}
                    className="rounded-md border border-white/10 px-2 py-1.5 text-sm text-base-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
                    aria-label="Previous search match"
                  >
                    ↑
                  </button>
                  <button
                    type="button"
                    onClick={() => showMatch(activeMatch + 1)}
                    className="rounded-md border border-white/10 px-2 py-1.5 text-sm text-base-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
                    aria-label="Next search match"
                  >
                    ↓
                  </button>
                </div>
              )}
            </form>
            <p className="min-h-4 text-xs text-base-300" aria-live="polite">
              {searchMessage}
            </p>

            <div
              ref={viewportRef}
              data-testid="prospectus-page-viewport"
              className="relative flex min-h-96 justify-center overflow-auto rounded-lg border border-white/10 bg-base-950 p-4"
              style={{ touchAction: "pan-y pinch-zoom" }}
            >
              <canvas
                ref={canvasRef}
                role="img"
                tabIndex={0}
                aria-label={`Rendered PDF page ${pageNumber} of ${pageCount}`}
                className="max-w-full bg-white shadow-lg"
              />
            </div>
            <p className="text-xs text-base-400">Swipe horizontally to change pages. Pinch to zoom.</p>
            {renderError && (
              <p role="alert" className="text-xs text-red-300">
                {renderError}
              </p>
            )}
          </>
        )}
      </div>
    </section>
  );
}

export default ProspectusViewer;
