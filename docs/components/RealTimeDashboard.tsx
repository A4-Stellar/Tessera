"use client";

import { useEffect, useId, useRef, useState } from "react";
import {
  CandlestickSeries,
  ColorType,
  HistogramSeries,
  createChart,
  createSeriesMarkers,
  type CandlestickData,
  type HistogramData,
  type SeriesMarker,
  type Time,
  type UTCTimestamp,
} from "lightweight-charts";
import { API_BASE_URL } from "@/lib/api";

/**
 * Live asset activity dashboard (issue #128).
 *
 * Streams contract events from `GET /v1/ws` into a TradingView
 * lightweight-charts candlestick + volume chart:
 *
 * - `valuation` events of the asset token are the price: one OHLC candle per
 *   interval, in USD (the contract stores cents).
 * - `transfer` events of the asset token are summed into the volume bars.
 * - `created` / `claim` events of the dividend contract become markers.
 *
 * Data never goes through React state. Messages are queued and folded into
 * the chart once per animation frame with `series.update()`, so a burst of
 * events costs one chart update per touched bar and no re-render. Only the
 * connection status badge is React state.
 *
 * On every (re)connect, live messages are buffered while `GET /v1/events`
 * backfills whatever was missed, then flushed. Events are deduplicated by
 * `(ledger, id)`, so the overlap between history and the live feed is
 * harmless. Dropped connections reconnect with capped exponential backoff.
 */

/** A contract event as streamed by `GET /v1/ws` and listed by `GET /v1/events`. */
export interface StreamEvent {
  id: number;
  contract: string;
  event_type: string;
  ledger: number;
  timestamp: string | null;
  data: Record<string, unknown>;
}

export interface RealTimeDashboardProps {
  /** Asset token contract (`C…`): its transfers drive volume, its valuation updates drive price. */
  assetContract: string;
  /** Dividend contract (`C…`): its `created` and `claim` events are marked on the chart. */
  dividendContract?: string;
  /** Token decimals used to scale transfer amounts. Defaults to 7. */
  decimals?: number;
  /** Candle width in seconds. Defaults to 60. */
  intervalSeconds?: number;
  /** Overrides `NEXT_PUBLIC_API_BASE_URL`. */
  apiBaseUrl?: string;
}

export type ConnectionStatus = "connecting" | "syncing" | "live" | "reconnecting";

const STATUS_LABEL: Record<ConnectionStatus, string> = {
  connecting: "Connecting…",
  syncing: "Syncing missed events…",
  live: "Live",
  reconnecting: "Reconnecting…",
};

/** Bars kept in memory and on the chart. */
const MAX_BARS = 2_000;
/** Event keys remembered for deduplication. */
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

/**
 * i128 amounts arrive as decimal strings. Since CAP-67 a transfer with muxed
 * destination info carries `{ amount, to_muxed_id }` instead of a bare amount.
 */
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
  /** Bars touched by the batch, in time order, for `series.update()`. */
  changed: Bar[];
  /** A batch reached behind the latest bar (backfill) or bars were trimmed: use `setData`. */
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
    private readonly decimals: number,
    private readonly intervalSeconds: number,
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
      // Trim in chunks so the full redraw this needs stays rare.
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
  signal: AbortSignal,
): Promise<StreamEvent[]> {
  const response = await fetch(`${apiRoot(baseUrl)}/v1/events`, { signal });
  if (!response.ok) throw new Error(`GET /v1/events returned ${response.status}`);
  const events: unknown = await response.json();
  return Array.isArray(events)
    ? events.filter(isStreamEvent).filter((event) => contracts.includes(event.contract))
    : [];
}

export default function RealTimeDashboard({
  assetContract,
  dividendContract,
  decimals = 7,
  intervalSeconds = 60,
  apiBaseUrl = API_BASE_URL,
}: RealTimeDashboardProps) {
  const headingId = useId();
  const containerRef = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<ConnectionStatus>("connecting");

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const chart = createChart(container, {
      autoSize: true,
      layout: { background: { type: ColorType.Solid, color: "transparent" }, textColor: "#a5abba" },
      grid: {
        vertLines: { color: "rgba(255, 255, 255, 0.06)" },
        horzLines: { color: "rgba(255, 255, 255, 0.06)" },
      },
      timeScale: { timeVisible: true, secondsVisible: false },
    });
    const candles = chart.addSeries(CandlestickSeries, {
      upColor: "#34d399",
      borderUpColor: "#34d399",
      wickUpColor: "#34d399",
      downColor: "#ef4444",
      borderDownColor: "#ef4444",
      wickDownColor: "#ef4444",
    });
    candles.priceScale().applyOptions({ scaleMargins: { top: 0.1, bottom: 0.3 } });
    const volume = chart.addSeries(HistogramSeries, {
      color: "rgba(56, 189, 248, 0.5)",
      priceFormat: { type: "volume" },
      priceScaleId: "",
    });
    volume.priceScale().applyOptions({ scaleMargins: { top: 0.75, bottom: 0 } });
    // Every marked interval has a volume bar (zero when it had no transfers).
    const markers = createSeriesMarkers(volume, []);

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
        candles.setData(bars.filter(hasPrice).map(toCandle));
        volume.setData(bars.map(toVolume));
      } else {
        for (const bar of changed) {
          if (hasPrice(bar)) candles.update(toCandle(bar));
          volume.update(toVolume(bar));
        }
      }
      if (markersChanged) markers.setMarkers(buckets.markers());
    };

    const enqueue = (events: StreamEvent[]) => {
      if (disposed) return;
      for (const event of events) {
        const key = `${event.ledger}:${event.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        if (seen.size > SEEN_CAPACITY) seen.delete(seen.values().next().value as string);
        queue.push(event);
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
      const ws = new WebSocket(toWebSocketUrl(apiBaseUrl));
      socket = ws;
      // Live messages received while history loads; flushed after it.
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

    // Skip the remaining backoff as soon as the browser is back online.
    const reconnectNow = () => {
      if (!socket && !disposed) {
        attempt = 0;
        connect();
      }
    };
    window.addEventListener("online", reconnectNow);
    connect();

    return () => {
      disposed = true;
      clearTimeout(retryTimer);
      cancelAnimationFrame(frame);
      history?.abort();
      window.removeEventListener("online", reconnectNow);
      socket?.close(1000);
      chart.remove();
    };
  }, [assetContract, dividendContract, decimals, intervalSeconds, apiBaseUrl]);

  return (
    <section
      aria-labelledby={headingId}
      className="rounded-xl border border-white/10 bg-base-900 p-4"
    >
      <header className="mb-3 flex flex-wrap items-center justify-between gap-3">
        <h3 id={headingId} className="text-base font-semibold text-base-50">
          Live asset activity
        </h3>
        <span
          role="status"
          aria-live="polite"
          className="inline-flex items-center gap-2 rounded-full border border-white/10 px-3 py-1 text-xs text-base-200"
        >
          <span
            aria-hidden="true"
            className={`h-2 w-2 rounded-full ${
              status === "live" ? "bg-brand-400" : "animate-pulse bg-gold-400"
            }`}
          />
          {STATUS_LABEL[status]}
        </span>
      </header>
      <div
        ref={containerRef}
        role="img"
        aria-label="Candlestick chart of asset valuation in USD with transfer volume bars and dividend markers"
        className="h-80 w-full"
      />
      <p className="mt-2 text-xs text-base-300">
        Candles: valuation (USD) per {intervalSeconds}s. Bars: transfer volume. Markers: dividend
        distributions and claims.
      </p>
    </section>
  );
}
