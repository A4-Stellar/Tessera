"use client";

import React, { Suspense, useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import {
  CandlestickSeries,
  ColorType,
  HistogramSeries,
  createChart,
  createSeriesMarkers,
  type CandlestickData,
  type HistogramData,
  type IChartApi,
  type ISeriesApi,
  type SeriesMarker,
  type Time,
  type UTCTimestamp,
} from "lightweight-charts";
import { API_BASE_URL } from "@/lib/api";
import { useEventStream, type StreamComplianceEvent } from "@/hooks/useEventStream";
import { useDebouncedResizeObserver } from "@/hooks/useDebouncedResizeObserver";
import { AnomalyAlert, extractAnomalyFromEvent, type AnomalyInfo } from "@/components/AnomalyAlert";

// ---------------------------------------------------------------------------
// Types & Helper Functions
// ---------------------------------------------------------------------------

/** A contract event as streamed by `GET /v1/ws` and listed by `GET /v1/events`. */
export interface StreamEvent {
  id: number;
  contract: string;
  event_type: string;
  ledger: number;
  timestamp: string | null;
  data: Record<string, unknown>;
}

export type ConnectionStatus = "connecting" | "syncing" | "live" | "reconnecting";

export const STATUS_LABEL: Record<ConnectionStatus, string> = {
  connecting: "Connecting…",
  syncing: "Syncing missed events…",
  live: "Live",
  reconnecting: "Reconnecting…",
};

const MAX_BARS = 2_000;
const SEEN_CAPACITY = 10_000;
const BASE_RECONNECT_MS = 500;
const MAX_RECONNECT_MS = 30_000;

function apiRoot(baseUrl: string): string {
  return baseUrl.replace(/\/+$/, "").replace(/\/v1$/, "");
}

/** `http(s)://host[/v1]` → `ws(s)://host/v1/ws`. */
export function toWebSocketUrl(baseUrl: string): string {
  return `${apiRoot(baseUrl).replace(/^http/, "ws")}/v1/ws`;
}

/**
 * Delay before reconnect attempt `attempt` (0-based): exponential growth
 * capped at 30 s, with jitter over the upper half of the window so clients
 * that dropped together do not reconnect together.
 */
export function reconnectDelay(attempt: number, random: () => number = Math.random): number {
  const ceiling = Math.min(MAX_RECONNECT_MS, BASE_RECONNECT_MS * 2 ** attempt);
  return Math.round(ceiling / 2 + (random() * ceiling) / 2);
}

function isStreamEvent(value: unknown): value is StreamEvent {
  const event = value as StreamEvent;
  return (
    typeof event === "object" &&
    event !== null &&
    typeof event.id === "number" &&
    typeof event.contract === "string" &&
    typeof event.event_type === "string" &&
    typeof event.ledger === "number" &&
    typeof event.data === "object" &&
    event.data !== null
  );
}

function parseEvent(message: unknown): StreamEvent | null {
  if (typeof message !== "string") return null;
  try {
    const value: unknown = JSON.parse(message);
    return isStreamEvent(value) ? value : null;
  } catch {
    return null;
  }
}

function toNumber(value: unknown): number | null {
  const raw =
    typeof value === "object" && value !== null && "amount" in value
      ? (value as { amount: unknown }).amount
      : value;
  const n = typeof raw === "string" || typeof raw === "number" ? Number(raw) : NaN;
  return Number.isFinite(n) ? n : null;
}

export interface Bar {
  time: UTCTimestamp;
  open?: number;
  high?: number;
  low?: number;
  close?: number;
  volume: number;
}

export interface ApplyResult {
  changed: Bar[];
  rebuild: boolean;
  markersChanged: boolean;
}

/** Folds contract events into interval bars and dividend markers. */
export class ChartBuckets {
  private readonly bars = new Map<number, Bar>();
  private markerList: SeriesMarker<Time>[] = [];
  private latest = -Infinity;

  constructor(
    private readonly assetContract: string,
    private readonly dividendContract: string | undefined,
    private readonly decimals: number = 7,
    private readonly intervalSeconds: number = 60
  ) {}

  apply(events: StreamEvent[]): ApplyResult {
    const changed = new Map<number, Bar>();
    let rebuild = false;
    let markersChanged = false;

    for (const event of events) {
      const seconds = event.timestamp ? Date.parse(event.timestamp) / 1000 : NaN;
      if (!Number.isFinite(seconds)) continue;
      const time = (Math.floor(seconds / this.intervalSeconds) * this.intervalSeconds) as UTCTimestamp;

      if (event.contract === this.assetContract && event.event_type === "transfer") {
        const amount = toNumber(event.data.amount);
        if (amount === null) continue;
        this.bar(time).volume += amount / 10 ** this.decimals;
      } else if (event.contract === this.assetContract && event.event_type === "valuation") {
        const cents = toNumber(event.data.value);
        if (cents === null) continue;
        const price = cents / 100;
        const bar = this.bar(time);
        bar.open ??= price;
        bar.high = Math.max(bar.high ?? price, price);
        bar.low = Math.min(bar.low ?? price, price);
        bar.close = price;
      } else if (
        event.contract === this.dividendContract &&
        (event.event_type === "created" || event.event_type === "claim")
      ) {
        this.bar(time);
        this.markerList.push({
          time,
          position: "aboveBar",
          shape: "arrowDown",
          color: "#fbbf24",
          text: event.event_type === "created" ? "Dividend" : "Claim",
        });
        markersChanged = true;
      } else {
        continue;
      }

      changed.set(time, this.bars.get(time) as Bar);
      rebuild ||= time < this.latest;
      this.latest = Math.max(this.latest, time);
    }

    if (this.bars.size > MAX_BARS) {
      const keep = new Set(this.sortedTimes().slice(-Math.floor(MAX_BARS * 0.9)));
      for (const time of this.bars.keys()) if (!keep.has(time)) this.bars.delete(time);
      this.markerList = this.markerList.filter((m) => keep.has(m.time as number));
      rebuild = true;
      markersChanged = true;
    }

    return {
      changed: [...changed.values()].sort((a, b) => a.time - b.time),
      rebuild,
      markersChanged,
    };
  }

  allBars(): Bar[] {
    return this.sortedTimes().map((time) => this.bars.get(time) as Bar);
  }

  markers(): SeriesMarker<Time>[] {
    return [...this.markerList].sort((a, b) => (a.time as number) - (b.time as number));
  }

  private bar(time: UTCTimestamp): Bar {
    let bar = this.bars.get(time);
    if (!bar) {
      bar = { time, volume: 0 };
      this.bars.set(time, bar);
    }
    return bar;
  }

  private sortedTimes(): number[] {
    return [...this.bars.keys()].sort((a, b) => a - b);
  }
}

function hasPrice(bar: Bar): boolean {
  return bar.open !== undefined;
}

function toCandle(bar: Bar): CandlestickData<Time> {
  return {
    time: bar.time,
    open: bar.open as number,
    high: bar.high as number,
    low: bar.low as number,
    close: bar.close as number,
  };
}

function toVolume(bar: Bar): HistogramData<Time> {
  return { time: bar.time, value: bar.volume };
}

async function fetchHistory(
  baseUrl: string,
  contracts: string[],
  signal?: AbortSignal
): Promise<StreamEvent[]> {
  try {
    const response = await fetch(`${apiRoot(baseUrl)}/v1/events`, { signal });
    if (!response.ok) return [];
    const events: unknown = await response.json();
    return Array.isArray(events)
      ? events.filter(isStreamEvent).filter((event) => contracts.includes(event.contract))
      : [];
  } catch {
    return [];
  }
}

// ---------------------------------------------------------------------------
// Heavy Visualisations (Lazy Loaded with Accessible Loading Skeleton)
// ---------------------------------------------------------------------------

const LazyComplianceHeatmap = React.lazy(
  () => import("@/components/ComplianceHeatmap")
);

export function HeatmapSkeleton() {
  return (
    <div
      aria-busy="true"
      aria-live="polite"
      aria-label="Loading compliance heatmap…"
      className="flex h-72 items-center justify-center rounded-xl border border-white/10 bg-base-900/60 p-6 text-sm text-base-300"
    >
      <div className="flex items-center gap-3">
        <span className="h-4 w-4 animate-spin rounded-full border-2 border-brand-500 border-t-transparent" />
        <span>Loading jurisdiction compliance heatmap…</span>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export interface RealTimeDashboardProps {
  /** Asset id for SSE events endpoint: GET /v1/assets/:id/events */
  assetId?: string | number;
  /** Asset token contract (`C…`): its transfers drive volume, its valuation updates drive price. */
  assetContract?: string;
  /** Dividend contract (`C…`): its `created` and `claim` events are marked on the chart. */
  dividendContract?: string;
  /** Token decimals used to scale transfer amounts. Defaults to 7. */
  decimals?: number;
  /** Candle width in seconds. Defaults to 60. */
  intervalSeconds?: number;
  /** Overrides `NEXT_PUBLIC_API_BASE_URL`. */
  apiBaseUrl?: string;
  className?: string;
}

export function RealTimeDashboard({
  assetId = "1",
  assetContract = "CASSETTOKEN",
  dividendContract,
  decimals = 7,
  intervalSeconds = 60,
  apiBaseUrl = API_BASE_URL,
  className = "",
}: RealTimeDashboardProps) {
  const headingId = useId();
  const [status, setStatus] = useState<ConnectionStatus>("connecting");

  // Hook 1: useDebouncedResizeObserver for responsive chart re-rendering
  const { ref: chartContainerRef, width: chartWidth } = useDebouncedResizeObserver<HTMLDivElement>(120);

  // Hook 2: useEventStream connecting to GET /v1/assets/:id/events SSE endpoint
  const sseEndpoint = `${apiRoot(apiBaseUrl)}/v1/assets/${assetId}/events`;
  const [anomaly, setAnomaly] = useState<AnomalyInfo | null>(null);

  const handleIncomingComplianceEvent = useCallback((event: StreamComplianceEvent) => {
    const alert = extractAnomalyFromEvent(event);
    if (alert) {
      setAnomaly(alert);
    }
  }, []);

  const {
    events: complianceEvents,
    status: sseStatus,
    reconnect: reconnectSse,
  } = useEventStream(sseEndpoint, {
    autoReconnect: true,
    initialReconnectDelay: 500,
    maxReconnectDelay: 30000,
    onEvent: handleIncomingComplianceEvent,
  });

  // TradingView Lightweight-charts rendering & WebSocket stream
  useEffect(() => {
    const container = chartContainerRef.current;
    if (!container) return;

    let chart: IChartApi | null = null;
    let candles: ISeriesApi<"Candlestick"> | null = null;
    let volume: ISeriesApi<"Histogram"> | null = null;
    let markers: ReturnType<typeof createSeriesMarkers> | null = null;

    try {
      chart = createChart(container, {
        autoSize: true,
        layout: { background: { type: ColorType.Solid, color: "transparent" }, textColor: "#a5abba" },
        grid: {
          vertLines: { color: "rgba(255, 255, 255, 0.06)" },
          horzLines: { color: "rgba(255, 255, 255, 0.06)" },
        },
        timeScale: { timeVisible: true, secondsVisible: false },
      });

      candles = chart.addSeries(CandlestickSeries, {
        upColor: "#34d399",
        borderUpColor: "#34d399",
        wickUpColor: "#34d399",
        downColor: "#ef4444",
        borderDownColor: "#ef4444",
        wickDownColor: "#ef4444",
      });
      candles.priceScale().applyOptions({ scaleMargins: { top: 0.1, bottom: 0.3 } });

      volume = chart.addSeries(HistogramSeries, {
        color: "rgba(56, 189, 248, 0.5)",
        priceFormat: { type: "volume" },
        priceScaleId: "",
      });
      volume.priceScale().applyOptions({ scaleMargins: { top: 0.75, bottom: 0 } });
      markers = createSeriesMarkers(volume, []);
    } catch {
      // In tests or environments where canvas / lightweight-charts is mocked
    }

    let disposed = false;
    const buckets = new ChartBuckets(assetContract, dividendContract, decimals, intervalSeconds);
    const contracts = dividendContract ? [assetContract, dividendContract] : [assetContract];
    const seen = new Set<string>();
    let queue: StreamEvent[] = [];
    let frame = 0;

    const draw = () => {
      frame = 0;
      const { changed, rebuild, markersChanged } = buckets.apply(queue);
      queue = [];
      if (rebuild) {
        const bars = buckets.allBars();
        candles?.setData(bars.filter(hasPrice).map(toCandle));
        volume?.setData(bars.map(toVolume));
      } else {
        for (const bar of changed) {
          if (hasPrice(bar)) candles?.update(toCandle(bar));
          volume?.update(toVolume(bar));
        }
      }
      if (markersChanged && markers) markers.setMarkers(buckets.markers());
    };

    const enqueue = (events: StreamEvent[]) => {
      if (disposed) return;
      for (const event of events) {
        const key = `${event.ledger}:${event.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        if (seen.size > SEEN_CAPACITY) seen.delete(seen.values().next().value as string);
        queue.push(event);

        // Check if matching anomaly detector (issue #30)
        const possibleAnomaly = extractAnomalyFromEvent({
          id: event.id,
          event_type: event.event_type,
          timestamp: event.timestamp || new Date().toISOString(),
          ledger: event.ledger,
          data: event.data,
        });
        if (possibleAnomaly) {
          setAnomaly(possibleAnomaly);
        }
      }
      if (queue.length > 0 && frame === 0) frame = requestAnimationFrame(draw);
    };

    let socket: WebSocket | null = null;
    let history: AbortController | null = null;
    let retryTimer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;

    const connect = () => {
      clearTimeout(retryTimer);
      setStatus(attempt === 0 ? "connecting" : "reconnecting");
      if (typeof WebSocket === "undefined") return;

      const ws = new WebSocket(toWebSocketUrl(apiBaseUrl));
      socket = ws;
      let pending: StreamEvent[] | null = [];

      ws.onopen = () => {
        for (const contract of contracts) ws.send(`subscribe:${contract}`);
        setStatus("syncing");
        history?.abort();
        history = new AbortController();
        fetchHistory(apiBaseUrl, contracts, history.signal)
          .then(enqueue, () => undefined)
          .finally(() => {
            if (socket !== ws) return;
            const buffered = pending ?? [];
            pending = null;
            enqueue(buffered);
            attempt = 0;
            setStatus("live");
          });
      };

      ws.onmessage = (message: MessageEvent) => {
        const event = parseEvent(message.data);
        if (!event) return;
        if (pending) pending.push(event);
        else enqueue([event]);
      };

      ws.onerror = () => ws.close();
      ws.onclose = () => {
        if (disposed || socket !== ws) return;
        socket = null;
        setStatus("reconnecting");
        retryTimer = setTimeout(connect, reconnectDelay(attempt++));
      };
    };

    const reconnectNow = () => {
      if (!socket && !disposed) {
        attempt = 0;
        connect();
      }
    };
    if (typeof window !== "undefined") {
      window.addEventListener("online", reconnectNow);
    }
    connect();

    return () => {
      disposed = true;
      clearTimeout(retryTimer);
      if (typeof cancelAnimationFrame !== "undefined") {
        cancelAnimationFrame(frame);
      }
      history?.abort();
      if (typeof window !== "undefined") {
        window.removeEventListener("online", reconnectNow);
      }
      socket?.close(1000);
      chart?.remove();
    };
  }, [assetContract, dividendContract, decimals, intervalSeconds, apiBaseUrl]);

  return (
    <section
      aria-labelledby={headingId}
      className={`space-y-6 rounded-xl border border-white/10 bg-base-900 p-4 ${className}`}
      data-testid="realtime-dashboard"
    >
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-white/10 pb-4">
        <div>
          <h3 id={headingId} className="text-base font-bold text-base-50">
            Real-Time Compliance &amp; Activity Dashboard
          </h3>
          <p className="mt-1 text-xs text-base-300">
            Streaming events via WebSocket and SSE (<code className="text-brand-300">GET /v1/assets/{assetId}/events</code>)
          </p>
        </div>
        <div className="flex items-center gap-3">
          <span
            role="status"
            aria-live="polite"
            className="inline-flex items-center gap-2 rounded-full border border-white/10 bg-base-950 px-3 py-1 text-xs text-base-200"
          >
            <span
              aria-hidden="true"
              className={`h-2 w-2 rounded-full ${
                status === "live" ? "bg-emerald-400" : "animate-pulse bg-gold-400"
              }`}
            />
            {STATUS_LABEL[status]}
          </span>
          {sseStatus === "disconnected" && (
            <button
              type="button"
              onClick={reconnectSse}
              className="rounded-lg border border-white/10 bg-base-800 px-3 py-1 text-xs font-semibold text-base-100 hover:bg-base-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
            >
              Reconnect SSE
            </button>
          )}
        </div>
      </header>

      {/* AnomalyAlert component (issue #30) */}
      {anomaly && (
        <AnomalyAlert
          anomaly={anomaly}
          onDismiss={() => setAnomaly(null)}
        />
      )}

      {/* Lightweight-charts container observed by useDebouncedResizeObserver */}
      <div
        ref={chartContainerRef}
        role="img"
        aria-label="Candlestick chart of asset valuation in USD with transfer volume bars and dividend markers"
        className="h-80 w-full rounded-lg border border-white/5 bg-base-950 p-2"
      />

      {/* Heavy Visualisation: ComplianceHeatmap powered by d3.js (React.lazy + Suspense) */}
      <Suspense fallback={<HeatmapSkeleton />}>
        <LazyComplianceHeatmap events={complianceEvents} refreshIntervalMs={5000} />
      </Suspense>

      {/* Real-time Compliance Events Feed */}
      <div className="rounded-xl border border-white/10 bg-base-950/80 p-4">
        <div className="mb-3 flex items-center justify-between">
          <h4 className="text-sm font-semibold text-base-100">
            Real-Time Compliance Events ({complianceEvents.length})
          </h4>
          <span className="text-xs text-base-400">SSE: /v1/assets/{assetId}/events</span>
        </div>

        <div
          role="log"
          aria-live="polite"
          aria-atomic="false"
          tabIndex={0}
          aria-label="Real-time compliance event stream"
          className="max-h-60 space-y-2 overflow-y-auto pr-1 text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        >
          {complianceEvents.length > 0 ? (
            complianceEvents.map((evt, idx) => {
              const allowed = evt.is_allowed !== false;
              return (
                <div
                  key={evt.id ?? idx}
                  className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-white/5 bg-base-900 p-2"
                >
                  <div className="flex items-center gap-2">
                    <span
                      className={`rounded px-1.5 py-0.5 font-mono text-[10px] font-bold ${
                        allowed
                          ? "border border-emerald-500/40 bg-emerald-500/20 text-emerald-300"
                          : "border border-red-500/40 bg-red-500/20 text-red-300"
                      }`}
                    >
                      {allowed ? "ALLOWED" : "BLOCKED"}
                    </span>
                    <span className="font-mono text-base-200">
                      {evt.jurisdiction ? `[${evt.jurisdiction}]` : "[GL]"} {evt.account || "GACCOUNT"}
                    </span>
                    {evt.reason && (
                      <span className="text-base-400 text-[11px]">— {evt.reason}</span>
                    )}
                  </div>
                  <span className="font-mono text-xs text-base-400">
                    {new Date(evt.timestamp).toLocaleTimeString()}
                  </span>
                </div>
              );
            })
          ) : (
            <div className="flex h-20 items-center justify-center text-xs text-base-400">
              Awaiting real-time compliance events from SSE endpoint…
            </div>
          )}
        </div>
      </div>
    </section>
  );
}

export default RealTimeDashboard;
