"use client";

import { useCallback, useMemo, useRef, useState } from "react";
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  ComposedChart,
  Line,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { API_BASE_URL } from "@/lib/api";
import { useWebSocket } from "@/hooks/useWebSocket";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Raw event shape returned by the /v1/ws WebSocket endpoint. */
interface WsEvent {
  id: number;
  contract: string;
  event_type: string;
  ledger: number;
  timestamp: string | null;
  data: Record<string, unknown>;
}

/** Parsed transfer event for the activity feed. */
interface TransferRecord {
  id: number;
  timestamp: string;
  from: string;
  to: string;
  amount: string;
  asset: string;
  ledger: number;
}

/** Parsed dividend event for the dividend feed. */
interface DividendRecord {
  id: number;
  timestamp: string;
  asset: string;
  amountPerShare: string;
  totalDistributed: string;
  ledger: number;
}

/** OHLC candlestick data point. */
interface CandleData {
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

/** Volume bar data point. */
interface VolumeBar {
  time: number;
  volume: number;
  color: string;
}

/** Aggregated dashboard statistics. */
interface DashboardStats {
  totalTransfers: number;
  totalVolume: number;
  totalDividends: number;
  activeAssets: number;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const CANDLE_INTERVAL_MS = 60_000; // 1-minute candles
const MAX_CANDLES = 120; // Keep last 2 hours of 1m candles
const MAX_FEED_ITEMS = 50;
const MAX_CHART_VOLUME_BARS = 60;

const CHART_COLORS = {
  bullish: "#34d399",
  bearish: "#f87171",
  volume: "#38bdf8",
  grid: "#1e293b",
  text: "#94a3b8",
  crosshair: "#475569",
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function truncateAddress(address: string): string {
  if (address.length <= 16) return address;
  return `${address.slice(0, 8)}…${address.slice(-6)}`;
}

function formatNumber(value: number, decimals = 2): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(decimals)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(decimals)}K`;
  return value.toFixed(decimals);
}

function formatTimestamp(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleTimeString("en-US", {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return iso;
  }
}

function getWsUrl(baseUrl: string): string {
  const trimmed = baseUrl.replace(/\/+$/, "");
  const httpBase = trimmed.endsWith("/v1")
    ? trimmed.slice(0, -3)
    : trimmed;
  const wsBase = httpBase.replace(/^http/, "ws");
  return `${wsBase}/v1/ws`;
}

function extractAmount(data: Record<string, unknown>): string {
  const raw = data.amount ?? data.value ?? data.balance ?? "0";
  return String(raw);
}

function extractAsset(data: Record<string, unknown>): string {
  const raw = data.asset ?? data.symbol ?? data.token ?? "Unknown";
  return String(raw);
}

function extractPrice(data: Record<string, unknown>): number | null {
  const raw = data.price ?? data.amount ?? data.value;
  if (raw == null) return null;
  const num = Number(raw);
  return Number.isFinite(num) ? num : null;
}

// ---------------------------------------------------------------------------
// Candlestick shape component for recharts
// ---------------------------------------------------------------------------

interface CandleWickProps {
  x?: number;
  y?: number;
  width?: number;
  height?: number;
  payload?: CandleData;
}

function CandleWick({ x = 0, y = 0, width = 0, height = 0, payload }: CandleWickProps) {
  if (!payload) return null;
  const { open, close, high, low } = payload;
  const isBullish = close >= open;
  const color = isBullish ? CHART_COLORS.bullish : CHART_COLORS.bearish;
  const centerX = x + width / 2;
  const bodyTop = y + (Math.max(open, close) / (high - low || 1)) * height;
  const bodyBottom = y + (Math.min(open, close) / (high - low || 1)) * height;
  const bodyHeight = Math.max(bodyBottom - bodyTop, 1);

  return (
    <g>
      {/* Wick (high-low line) */}
      <line
        x1={centerX}
        y1={y}
        x2={centerX}
        y2={y + height}
        stroke={color}
        strokeWidth={1}
      />
      {/* Body (open-close rectangle) */}
      <rect
        x={x + width * 0.15}
        y={bodyTop}
        width={width * 0.7}
        height={bodyHeight}
        fill={color}
        stroke={color}
        strokeWidth={1}
        rx={1}
      />
    </g>
  );
}

// ---------------------------------------------------------------------------
// Custom tooltip components
// ---------------------------------------------------------------------------

interface TooltipPayloadItem {
  payload?: CandleData;
}

function CandleTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: TooltipPayloadItem[];
}) {
  if (!active || !payload?.length) return null;
  const candle = payload[0].payload;
  if (!candle) return null;

  const isBullish = candle.close >= candle.open;
  const change = candle.close - candle.open;
  const changePct = candle.open !== 0 ? (change / candle.open) * 100 : 0;

  return (
    <div className="rounded-lg border border-base-600 bg-base-850 px-3 py-2 text-xs shadow-xl">
      <div className="mb-1 font-mono text-[10px] text-base-300">
        {new Date(candle.time).toLocaleTimeString()}
      </div>
      <div className="grid grid-cols-2 gap-x-4 gap-y-0.5">
        <span className="text-base-300">Open</span>
        <span className="text-right font-mono text-base-100">
          {candle.open.toFixed(4)}
        </span>
        <span className="text-base-300">High</span>
        <span className="text-right font-mono text-emerald-400">
          {candle.high.toFixed(4)}
        </span>
        <span className="text-base-300">Low</span>
        <span className="text-right font-mono text-red-400">
          {candle.low.toFixed(4)}
        </span>
        <span className="text-base-300">Close</span>
        <span className="text-right font-mono text-base-100">
          {candle.close.toFixed(4)}
        </span>
        <span className="text-base-300">Change</span>
        <span
          className={`text-right font-mono ${isBullish ? "text-emerald-400" : "text-red-400"}`}
        >
          {change >= 0 ? "+" : ""}
          {change.toFixed(4)} ({changePct >= 0 ? "+" : ""}
          {changePct.toFixed(2)}%)
        </span>
        <span className="text-base-300">Volume</span>
        <span className="text-right font-mono text-sky-400">
          {formatNumber(candle.volume)}
        </span>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Stat card component
// ---------------------------------------------------------------------------

interface StatCardProps {
  label: string;
  value: string;
  subtext?: string;
  accent?: "emerald" | "sky" | "amber" | "violet";
}

function StatCard({ label, value, subtext, accent = "emerald" }: StatCardProps) {
  const accentMap = {
    emerald: "text-emerald-400",
    sky: "text-sky-400",
    amber: "text-amber-400",
    violet: "text-violet-400",
  };

  return (
    <div className="rounded-xl border border-base-600/50 bg-base-800/50 p-4">
      <div className="text-xs font-medium uppercase tracking-wider text-base-300">
        {label}
      </div>
      <div className={`mt-1 text-2xl font-bold ${accentMap[accent]}`}>
        {value}
      </div>
      {subtext && <div className="mt-0.5 text-xs text-base-300">{subtext}</div>}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Connection status badge
// ---------------------------------------------------------------------------

function ConnectionBadge({
  status,
  reconnectAttempts,
}: {
  status: string;
  reconnectAttempts: number;
}) {
  const statusConfig: Record<string, { color: string; label: string }> = {
    connecting: { color: "bg-amber-400", label: "Connecting…" },
    open: { color: "bg-emerald-400", label: "Live" },
    closing: { color: "bg-amber-400", label: "Closing…" },
    closed: { color: "bg-red-400", label: "Disconnected" },
    reconnecting: {
      color: "bg-amber-400",
      label: `Reconnecting (${reconnectAttempts})…`,
    },
  };

  const config = statusConfig[status] ?? statusConfig.closed;

  return (
    <div className="flex items-center gap-2 rounded-full border border-base-600/50 bg-base-800/50 px-3 py-1.5">
      <span className="relative flex h-2.5 w-2.5">
        {status === "open" && (
          <span
            className={`absolute inline-flex h-full w-full animate-ping rounded-full ${config.color} opacity-60`}
          />
        )}
        <span
          className={`relative inline-flex h-2.5 w-2.5 rounded-full ${config.color}`}
        />
      </span>
      <span className="text-xs font-medium text-base-100">{config.label}</span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export interface RealTimeDashboardProps {
  /** Overrides NEXT_PUBLIC_API_BASE_URL for demos/tests. */
  apiBaseUrl?: string;
  /** WebSocket subscription topics (comma-separated). */
  topics?: string;
  /** Maximum number of candles to display. */
  maxCandles?: number;
}

/**
 * Real-time streaming dashboard displaying live asset transfer activity,
 * price fluctuations (candlestick chart), and dividend events via WebSocket.
 *
 * Connects to `GET /v1/ws` and subscribes to specified topics. New data
 * points are smoothly appended to charts without full page re-renders.
 * Connection drops trigger automatic reconnection with exponential backoff
 * and missed-message buffering.
 */
export function RealTimeDashboard({
  apiBaseUrl = API_BASE_URL,
  topics = "transfer,dividend,price",
  maxCandles = MAX_CANDLES,
}: RealTimeDashboardProps) {
  const wsUrl = useMemo(() => getWsUrl(apiBaseUrl), [apiBaseUrl]);

  // Chart data stored in refs for smooth appending without re-renders
  const candlesRef = useRef<CandleData[]>([]);
  const volumeBarsRef = useRef<VolumeBar[]>([]);
  const [chartData, setChartData] = useState<{
    candles: CandleData[];
    volumes: VolumeBar[];
  }>({ candles: [], volumes: [] });

  // Feed data
  const [transfers, setTransfers] = useState<TransferRecord[]>([]);
  const [dividends, setDividends] = useState<DividendRecord[]>([]);
  const [stats, setStats] = useState<DashboardStats>({
    totalTransfers: 0,
    totalVolume: 0,
    totalDividends: 0,
    activeAssets: 0,
  });

  // Track which assets we've seen for the active count
  const seenAssetsRef = useRef<Set<string>>(new Set());

  // Throttle chart state updates to avoid excessive re-renders
  const lastChartUpdateRef = useRef(0);
  const CHART_UPDATE_THROTTLE_MS = 250;

  const flushChartData = useCallback(() => {
    const now = Date.now();
    if (now - lastChartUpdateRef.current < CHART_UPDATE_THROTTLE_MS) return;
    lastChartUpdateRef.current = now;
    setChartData({
      candles: [...candlesRef.current],
      volumes: [...volumeBarsRef.current],
    });
  }, []);

  const processEvent = useCallback(
    (raw: unknown) => {
      if (typeof raw !== "object" || raw === null) return;
      const event = raw as WsEvent;
      const eventType = event.event_type?.toLowerCase() ?? "";
      const data = event.data ?? {};
      const timestamp = event.timestamp ?? new Date().toISOString();
      const asset = extractAsset(data);
      const amount = extractAmount(data);

      // Update seen assets
      seenAssetsRef.current.add(asset);

      // Handle transfer events
      if (eventType.includes("transfer") || eventType.includes("payment")) {
        const transfer: TransferRecord = {
          id: event.id,
          timestamp,
          from: truncateAddress(String(data.from ?? data.sender ?? "—")),
          to: truncateAddress(String(data.to ?? data.recipient ?? "—")),
          amount,
          asset,
          ledger: event.ledger,
        };

        setTransfers((prev) => [transfer, ...prev].slice(0, MAX_FEED_ITEMS));

        const numericAmount = Number(amount) || 0;
        setStats((prev) => ({
          ...prev,
          totalTransfers: prev.totalTransfers + 1,
          totalVolume: prev.totalVolume + numericAmount,
          activeAssets: seenAssetsRef.current.size,
        }));
      }

      // Handle dividend events
      if (eventType.includes("dividend") || eventType.includes("distribution")) {
        const dividend: DividendRecord = {
          id: event.id,
          timestamp,
          asset,
          amountPerShare: String(data.amount_per_share ?? data.amount ?? "0"),
          totalDistributed: String(data.total ?? data.total_distributed ?? "0"),
          ledger: event.ledger,
        };

        setDividends((prev) => [dividend, ...prev].slice(0, MAX_FEED_ITEMS));

        setStats((prev) => ({
          ...prev,
          totalDividends: prev.totalDividends + 1,
          activeAssets: seenAssetsRef.current.size,
        }));
      }

      // Handle price events — aggregate into candles
      const price = extractPrice(data);
      if (price != null && price > 0) {
        const candleTime =
          Math.floor(new Date(timestamp).getTime() / CANDLE_INTERVAL_MS) *
          CANDLE_INTERVAL_MS;

        const candles = candlesRef.current;
        const lastCandle = candles[candles.length - 1];

        if (lastCandle && lastCandle.time === candleTime) {
          // Update existing candle
          lastCandle.high = Math.max(lastCandle.high, price);
          lastCandle.low = Math.min(lastCandle.low, price);
          lastCandle.close = price;
          lastCandle.volume += Number(amount) || 0;
        } else {
          // Create new candle
          const newCandle: CandleData = {
            time: candleTime,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: Number(amount) || 0,
          };
          candles.push(newCandle);

          // Trim old candles
          if (candles.length > maxCandles) {
            candles.splice(0, candles.length - maxCandles);
          }
        }

        // Update volume bars
        const volumeBars = volumeBarsRef.current;
        const lastBar = volumeBars[volumeBars.length - 1];
        const barColor =
          price >= (lastCandle?.open ?? price)
            ? CHART_COLORS.bullish
            : CHART_COLORS.bearish;

        if (lastBar && lastBar.time === candleTime) {
          lastBar.volume += Number(amount) || 0;
          lastBar.color = barColor;
        } else {
          volumeBars.push({
            time: candleTime,
            volume: Number(amount) || 0,
            color: barColor,
          });
          if (volumeBars.length > MAX_CHART_VOLUME_BARS) {
            volumeBars.splice(0, volumeBars.length - MAX_CHART_VOLUME_BARS);
          }
        }

        flushChartData();
      }
    },
    [maxCandles, flushChartData],
  );

  const handleMessage = useCallback(
    (data: unknown) => {
      processEvent(data);
    },
    [processEvent],
  );

  const { status, isConnected, send, reconnect, reconnectAttempts, bufferedMessages } =
    useWebSocket({
      url: wsUrl,
      onMessage: handleMessage,
      onOpen: () => {
        // Send subscriptions after connection opens
        const topicList = topics
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean);
        topicList.forEach((topic) => {
          send(`subscribe:${topic}`);
        });
      },
      autoReconnect: true,
      initialReconnectDelay: 1_000,
      maxReconnectDelay: 30_000,
      maxReconnectAttempts: 10,
    });

  // ---------------------------------------------------------------------------
  // Chart data preparation
  // ---------------------------------------------------------------------------

  const chartCandles = chartData.candles;
  const chartVolumes = chartData.volumes;

  // Calculate price range for YAxis domain
  const priceDomain = useMemo(() => {
    if (chartCandles.length === 0) return [0, 1] as [number, number];
    let min = Infinity;
    let max = -Infinity;
    for (const c of chartCandles) {
      if (c.low < min) min = c.low;
      if (c.high > max) max = c.high;
    }
    const padding = (max - min) * 0.05 || 0.01;
    return [min - padding, max + padding] as [number, number];
  }, [chartCandles]);

  const volumeDomain = useMemo(() => {
    if (chartVolumes.length === 0) return [0, 1] as [number, number];
    let max = 0;
    for (const v of chartVolumes) {
      if (v.volume > max) max = v.volume;
    }
    return [0, max * 1.1 || 1] as [number, number];
  }, [chartVolumes]);

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h2 className="text-xl font-bold text-base-50">Real-Time Dashboard</h2>
          <p className="text-sm text-base-300">
            Live asset transfers, price activity, and dividend events
          </p>
        </div>
        <div className="flex items-center gap-3">
          {bufferedMessages.length > 0 && (
            <span className="rounded-full border border-amber-500/30 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-300">
              {bufferedMessages.length} buffered
            </span>
          )}
          <ConnectionBadge status={status} reconnectAttempts={reconnectAttempts} />
          {status === "closed" && (
            <button
              onClick={reconnect}
              className="rounded-lg border border-base-600 bg-base-800 px-3 py-1.5 text-xs font-medium text-base-100 transition-colors hover:bg-base-700"
            >
              Reconnect
            </button>
          )}
        </div>
      </div>

      {/* Stats row */}
      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <StatCard
          label="Total Transfers"
          value={stats.totalTransfers.toLocaleString()}
          accent="emerald"
        />
        <StatCard
          label="Total Volume"
          value={formatNumber(stats.totalVolume)}
          accent="sky"
        />
        <StatCard
          label="Dividend Events"
          value={stats.totalDividends.toLocaleString()}
          accent="amber"
        />
        <StatCard
          label="Active Assets"
          value={stats.activeAssets.toLocaleString()}
          accent="violet"
        />
      </div>

      {/* Charts row */}
      <div className="grid gap-6 lg:grid-cols-3">
        {/* Candlestick chart */}
        <div className="rounded-xl border border-base-600/50 bg-base-800/50 p-4 lg:col-span-2">
          <div className="mb-3 flex items-center justify-between">
            <h3 className="text-sm font-semibold text-base-100">
              Price Chart (1m candles)
            </h3>
            <span className="text-xs text-base-300">
              {chartCandles.length} candles
            </span>
          </div>
          {chartCandles.length > 0 ? (
            <ResponsiveContainer width="100%" height={300}>
              <ComposedChart data={chartCandles} margin={{ top: 5, right: 5, bottom: 5, left: 5 }}>
                <CartesianGrid stroke={CHART_COLORS.grid} strokeDasharray="3 3" />
                <XAxis
                  dataKey="time"
                  tickFormatter={(val: number) =>
                    new Date(val).toLocaleTimeString("en-US", {
                      hour: "2-digit",
                      minute: "2-digit",
                    })
                  }
                  stroke={CHART_COLORS.text}
                  tick={{ fill: CHART_COLORS.text, fontSize: 10 }}
                  minTickGap={30}
                />
                <YAxis
                  domain={priceDomain}
                  stroke={CHART_COLORS.text}
                  tick={{ fill: CHART_COLORS.text, fontSize: 10 }}
                  width={60}
                  tickFormatter={(val: number) => val.toFixed(4)}
                />
                <Tooltip content={<CandleTooltip />} />
                <Bar dataKey="high" shape={<CandleWick />} isAnimationActive={false} />
              </ComposedChart>
            </ResponsiveContainer>
          ) : (
            <div className="flex h-[300px] items-center justify-center text-sm text-base-300">
              {isConnected
                ? "Waiting for price data…"
                : "Connect to the WebSocket to see live price data."}
            </div>
          )}
        </div>

        {/* Volume chart */}
        <div className="rounded-xl border border-base-600/50 bg-base-800/50 p-4">
          <div className="mb-3 flex items-center justify-between">
            <h3 className="text-sm font-semibold text-base-100">Volume</h3>
            <span className="text-xs text-base-300">
              {chartVolumes.length} bars
            </span>
          </div>
          {chartVolumes.length > 0 ? (
            <ResponsiveContainer width="100%" height={300}>
              <BarChart data={chartVolumes} margin={{ top: 5, right: 5, bottom: 5, left: 5 }}>
                <CartesianGrid stroke={CHART_COLORS.grid} strokeDasharray="3 3" />
                <XAxis
                  dataKey="time"
                  tickFormatter={(val: number) =>
                    new Date(val).toLocaleTimeString("en-US", {
                      hour: "2-digit",
                      minute: "2-digit",
                    })
                  }
                  stroke={CHART_COLORS.text}
                  tick={{ fill: CHART_COLORS.text, fontSize: 10 }}
                  minTickGap={30}
                />
                <YAxis
                  domain={volumeDomain}
                  stroke={CHART_COLORS.text}
                  tick={{ fill: CHART_COLORS.text, fontSize: 10 }}
                  width={50}
                  tickFormatter={(val: number) => formatNumber(val, 0)}
                />
                <Tooltip
                  contentStyle={{
                    backgroundColor: "#11141b",
                    border: "1px solid #333a49",
                    borderRadius: "8px",
                    fontSize: "12px",
                  }}
                  labelFormatter={(val: number) =>
                    new Date(val).toLocaleTimeString()
                  }
                  formatter={(value: number) => [formatNumber(value), "Volume"]}
                />
                <Bar dataKey="volume" isAnimationActive={false}>
                  {chartVolumes.map((bar, index) => (
                    <Cell key={index} fill={bar.color} fillOpacity={0.7} />
                  ))}
                </Bar>
              </BarChart>
            </ResponsiveContainer>
          ) : (
            <div className="flex h-[300px] items-center justify-center text-sm text-base-300">
              {isConnected
                ? "Waiting for volume data…"
                : "Connect to the WebSocket to see live volume data."}
            </div>
          )}
        </div>
      </div>

      {/* Feeds row */}
      <div className="grid gap-6 lg:grid-cols-2">
        {/* Transfer activity feed */}
        <div className="rounded-xl border border-base-600/50 bg-base-800/50 p-4">
          <div className="mb-3 flex items-center justify-between">
            <h3 className="text-sm font-semibold text-base-100">
              Transfer Activity
            </h3>
            <span className="text-xs text-base-300">
              {transfers.length} events
            </span>
          </div>
          {transfers.length > 0 ? (
            <div className="max-h-[300px] space-y-2 overflow-y-auto pr-1">
              {transfers.map((t) => (
                <div
                  key={t.id}
                  className="flex items-center justify-between rounded-lg border border-base-600/30 bg-base-800/30 px-3 py-2 text-xs"
                >
                  <div className="flex items-center gap-2">
                    <span className="rounded bg-emerald-500/10 px-1.5 py-0.5 font-mono text-[10px] text-emerald-400">
                      {t.asset}
                    </span>
                    <span className="font-mono text-base-200">
                      {t.from} → {t.to}
                    </span>
                  </div>
                  <div className="flex items-center gap-3">
                    <span className="font-mono text-sky-400">
                      {formatNumber(Number(t.amount), 4)}
                    </span>
                    <span className="text-base-300">
                      {formatTimestamp(t.timestamp)}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          ) : (
            <div className="flex h-[200px] items-center justify-center text-sm text-base-300">
              {isConnected
                ? "Waiting for transfer events…"
                : "Connect to the WebSocket to see live transfers."}
            </div>
          )}
        </div>

        {/* Dividend events feed */}
        <div className="rounded-xl border border-base-600/50 bg-base-800/50 p-4">
          <div className="mb-3 flex items-center justify-between">
            <h3 className="text-sm font-semibold text-base-100">
              Dividend Events
            </h3>
            <span className="text-xs text-base-300">
              {dividends.length} events
            </span>
          </div>
          {dividends.length > 0 ? (
            <div className="max-h-[300px] space-y-2 overflow-y-auto pr-1">
              {dividends.map((d) => (
                <div
                  key={d.id}
                  className="flex items-center justify-between rounded-lg border border-base-600/30 bg-base-800/30 px-3 py-2 text-xs"
                >
                  <div className="flex items-center gap-2">
                    <span className="rounded bg-amber-500/10 px-1.5 py-0.5 font-mono text-[10px] text-amber-400">
                      {d.asset}
                    </span>
                    <span className="text-base-200">Distribution</span>
                  </div>
                  <div className="flex items-center gap-3">
                    <span className="font-mono text-amber-400">
                      {formatNumber(Number(d.totalDistributed), 4)} total
                    </span>
                    <span className="text-base-300">
                      {formatTimestamp(d.timestamp)}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          ) : (
            <div className="flex h-[200px] items-center justify-center text-sm text-base-300">
              {isConnected
                ? "Waiting for dividend events…"
                : "Connect to the WebSocket to see live dividends."}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
