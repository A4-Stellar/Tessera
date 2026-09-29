import { act, render, screen } from "@testing-library/react";
import RealTimeDashboard, {
  ChartBuckets,
  reconnectDelay,
  toWebSocketUrl,
  type StreamEvent,
} from "../../components/RealTimeDashboard";

const mockCandles = {
  update: jest.fn(),
  setData: jest.fn(),
  priceScale: () => ({ applyOptions: jest.fn() }),
};
const mockVolume = {
  update: jest.fn(),
  setData: jest.fn(),
  priceScale: () => ({ applyOptions: jest.fn() }),
};
const mockSetMarkers = jest.fn();
const mockRemove = jest.fn();

// lightweight-charts is ESM-only and draws on <canvas>, which jsdom lacks.
// `virtual` because its package exports have no CommonJS entry for Jest.
jest.mock(
  "lightweight-charts",
  () => ({
    CandlestickSeries: "Candlestick",
    HistogramSeries: "Histogram",
    ColorType: { Solid: "solid" },
    createChart: jest.fn(() => ({
      addSeries: (type: string) => (type === "Candlestick" ? mockCandles : mockVolume),
      remove: mockRemove,
    })),
    createSeriesMarkers: jest.fn(() => ({ setMarkers: mockSetMarkers })),
  }),
  { virtual: true },
);

const ASSET = "CASSETTOKEN";
const DIVIDEND = "CDIVIDEND";
const T0 = Date.parse("2026-09-28T12:00:00Z");

function event(
  id: number,
  secondsAfterT0: number,
  event_type: string,
  data: Record<string, unknown>,
  contract = ASSET,
): StreamEvent {
  return {
    id,
    contract,
    event_type,
    ledger: 1_000 + id,
    timestamp: new Date(T0 + secondsAfterT0 * 1_000).toISOString(),
    data,
  };
}

const transfer = (id: number, s: number, amount: unknown) => event(id, s, "transfer", { amount });
const valuation = (id: number, s: number, cents: string) => event(id, s, "valuation", { value: cents });

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  send = jest.fn();
  closed = false;

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  open() {
    this.onopen?.();
  }

  emit(value: StreamEvent) {
    this.onmessage?.({ data: JSON.stringify(value) });
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.onclose?.();
  }
}

/** Let the history fetch promise chain settle. */
async function settle() {
  await act(async () => {
    for (let i = 0; i < 10; i += 1) await Promise.resolve();
  });
}

describe("RealTimeDashboard helpers", () => {
  it("derives the WebSocket endpoint from the API base URL", () => {
    expect(toWebSocketUrl("http://localhost:8080")).toBe("ws://localhost:8080/v1/ws");
    expect(toWebSocketUrl("https://api.example.com/v1/")).toBe("wss://api.example.com/v1/ws");
  });

  it("backs off exponentially with jitter and a 30s cap", () => {
    expect(reconnectDelay(0, () => 0)).toBe(250);
    expect(reconnectDelay(0, () => 1)).toBe(500);
    expect(reconnectDelay(3, () => 0)).toBe(2_000);
    expect(reconnectDelay(20, () => 1)).toBe(30_000);
  });

  it("folds valuations into OHLC candles and transfers into volume", () => {
    const buckets = new ChartBuckets(ASSET, DIVIDEND, 7, 60);
    const result = buckets.apply([
      valuation(1, 0, "10000"),
      valuation(2, 10, "12500"),
      valuation(3, 20, "9000"),
      valuation(4, 30, "11000"),
      transfer(5, 40, "25000000"),
      // CAP-67 muxed transfer data.
      transfer(6, 50, { amount: "5000000", to_muxed_id: 7 }),
      // Another contract's transfer is ignored.
      event(7, 55, "transfer", { amount: "1" }, "COTHER"),
    ]);

    expect(result.rebuild).toBe(false);
    expect(result.changed).toEqual([
      { time: T0 / 1_000, open: 100, high: 125, low: 90, close: 110, volume: 3 },
    ]);
  });

  it("marks dividend events and flags out-of-order bars for a rebuild", () => {
    const buckets = new ChartBuckets(ASSET, DIVIDEND, 7, 60);
    buckets.apply([transfer(1, 120, "10000000")]);

    const result = buckets.apply([
      event(2, 0, "created", { value: ["1", "500"] }, DIVIDEND),
    ]);

    expect(result.rebuild).toBe(true);
    expect(result.markersChanged).toBe(true);
    expect(buckets.markers()).toEqual([
      expect.objectContaining({ time: T0 / 1_000, text: "Dividend" }),
    ]);
    expect(buckets.allBars().map((bar) => bar.time)).toEqual([T0 / 1_000, T0 / 1_000 + 120]);
  });
});

describe("RealTimeDashboard", () => {
  const originalWebSocket = global.WebSocket;
  const originalFetch = global.fetch;
  let history: StreamEvent[];

  beforeEach(() => {
    jest.useFakeTimers();
    jest.clearAllMocks();
    FakeWebSocket.instances = [];
    history = [];
    global.WebSocket = FakeWebSocket as unknown as typeof WebSocket;
    global.fetch = jest.fn(() =>
      Promise.resolve({ ok: true, json: () => Promise.resolve(history) }),
    ) as unknown as typeof fetch;
  });

  afterEach(() => {
    jest.useRealTimers();
    global.WebSocket = originalWebSocket;
    global.fetch = originalFetch;
  });

  it("subscribes, backfills missed events and buffers live ones during sync", async () => {
    history = [transfer(1, 0, "10000000"), transfer(2, 5, "10000000")];
    render(
      <RealTimeDashboard
        assetContract={ASSET}
        dividendContract={DIVIDEND}
        apiBaseUrl="http://api.test"
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Connecting…");

    const ws = FakeWebSocket.instances[0];
    expect(ws.url).toBe("ws://api.test/v1/ws");
    act(() => ws.open());
    expect(ws.send).toHaveBeenCalledWith(`subscribe:${ASSET}`);
    expect(ws.send).toHaveBeenCalledWith(`subscribe:${DIVIDEND}`);
    expect(global.fetch).toHaveBeenCalledWith("http://api.test/v1/events", expect.anything());
    expect(screen.getByRole("status")).toHaveTextContent("Syncing missed events…");

    // Arrives while history is loading: held, and a duplicate of history.
    act(() => ws.emit(transfer(2, 5, "10000000")));
    act(() => ws.emit(transfer(3, 70, "30000000")));
    await settle();
    expect(screen.getByRole("status")).toHaveTextContent("Live");

    act(() => {
      jest.advanceTimersByTime(20);
    });
    expect(mockVolume.update.mock.calls.map(([bar]) => bar)).toEqual([
      { time: T0 / 1_000, value: 2 },
      { time: T0 / 1_000 + 60, value: 3 },
    ]);

    // Live events after sync go straight to the chart on the next frame.
    act(() => ws.emit(valuation(4, 80, "20000")));
    act(() => {
      jest.advanceTimersByTime(20);
    });
    expect(mockCandles.update).toHaveBeenLastCalledWith({
      time: T0 / 1_000 + 60,
      open: 200,
      high: 200,
      low: 200,
      close: 200,
    });
  });

  it("reconnects after a dropped connection and cleans up on unmount", async () => {
    const { unmount } = render(<RealTimeDashboard assetContract={ASSET} apiBaseUrl="http://api.test" />);
    const first = FakeWebSocket.instances[0];
    act(() => first.open());
    await settle();

    act(() => first.close());
    expect(screen.getByRole("status")).toHaveTextContent("Reconnecting…");
    expect(FakeWebSocket.instances).toHaveLength(1);

    act(() => {
      jest.advanceTimersByTime(500);
    });
    expect(FakeWebSocket.instances).toHaveLength(2);
    const second = FakeWebSocket.instances[1];
    act(() => second.open());
    await settle();
    expect(second.send).toHaveBeenCalledWith(`subscribe:${ASSET}`);
    expect(screen.getByRole("status")).toHaveTextContent("Live");

    unmount();
    expect(second.closed).toBe(true);
    expect(mockRemove).toHaveBeenCalled();
    act(() => {
      jest.advanceTimersByTime(60_000);
    });
    expect(FakeWebSocket.instances).toHaveLength(2);
  });
});
