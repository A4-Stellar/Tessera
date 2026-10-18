import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { axe } from "jest-axe";
import {
  CapTableCompare,
  SAMPLE_COMPARE_ASSETS,
  loadCompareAsset,
} from "../../components/CapTableCompare";

class MockWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances: MockWebSocket[] = [];

  url: string;
  readyState = MockWebSocket.CONNECTING;
  onopen: ((event?: unknown) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event?: unknown) => void) | null = null;
  onerror: ((event?: unknown) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    MockWebSocket.instances.push(this);
  }

  send(): void {}
  close(): void {
    this.readyState = MockWebSocket.CLOSED;
  }
}

const originalWebSocket = global.WebSocket;
const originalFetch = global.fetch;

function jsonResponse(body: unknown) {
  return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(body) });
}

const API_ASSETS = [
  { id: 1, token_contract: "CLOFT", name: "Loft", symbol: "LOFT", asset_type: "real_estate", valuation_usd: 1_000_000, decimals: 7, total_supply: "10000000000", holders: 3, indexed_at_ledger: 10_000_000 },
  { id: 2, token_contract: "CGOLD", name: "Gold", symbol: "GOLD", asset_type: "commodity", valuation_usd: 500_000, decimals: 7, total_supply: "50000000000", holders: 2, indexed_at_ledger: 10_000_000 },
];

function apiFetch(url: string) {
  if (url.endsWith("/v1/assets?active=true")) return jsonResponse(API_ASSETS);
  const id = /\/v1\/assets\/(\d+)\//.exec(url)?.[1];
  if (url.endsWith("/analytics")) return jsonResponse({ gini_coefficient: id === "1" ? 0.25 : null });
  if (url.includes("/holders")) {
    return jsonResponse(id === "1"
      ? [{ balance: "500", share_percent: 50 }, { balance: "300", share_percent: 30 }, { balance: "200", share_percent: 20 }]
      : [{ balance: "900", share_percent: 90 }, { balance: "100", share_percent: 10 }]);
  }
  if (url.endsWith("/dividends")) {
    return jsonResponse(id === "1"
      ? [
          { created_at_ledger: 10_000_000 - 91 * 17_280, fiat_equivalent_usd: 10_000 },
          { created_at_ledger: 10_000_000 - 1 * 17_280, fiat_equivalent_usd: 10_000 },
        ]
      : []);
  }
  if (url.endsWith("/compliance")) {
    return jsonResponse({ total_records: 4, approved: 3, jurisdictions: [{ jurisdiction: "US", count: 3 }, { jurisdiction: "DE", count: 1 }] });
  }
  return Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({}) });
}

describe("CapTableCompare", () => {
  beforeEach(() => {
    MockWebSocket.instances = [];
    (global as unknown as { WebSocket: unknown }).WebSocket = MockWebSocket;
  });

  afterEach(() => {
    (global as unknown as { WebSocket: unknown }).WebSocket = originalWebSocket;
    global.fetch = originalFetch;
  });

  it("compares the first two sample assets side by side and labels them as sample data", () => {
    render(<CapTableCompare />);

    expect(screen.getByText(/illustrative sample data/i)).toBeInTheDocument();
    const table = screen.getByRole("table");
    expect(within(table).getByRole("columnheader", { name: /Manhattan Loft/ })).toBeInTheDocument();
    expect(within(table).getByRole("columnheader", { name: /Invoice Pool/ })).toBeInTheDocument();
    expect(within(within(table).getAllByRole("row")[0]).getAllByRole("columnheader")).toHaveLength(3);
  });

  it("highlights Gini, minimum investment and dividend schedule differences against the baseline", () => {
    render(<CapTableCompare />);

    const giniRow = screen.getByRole("rowheader", { name: "Gini coefficient" }).closest("tr")!;
    const [, invcGini] = within(giniRow).getAllByRole("cell");
    expect(invcGini).toHaveAttribute("data-differs", "true");
    expect(invcGini).toHaveTextContent("0.710");
    expect(invcGini).toHaveTextContent("+0.330 vs LOFT");
    expect(invcGini).toHaveTextContent("Least favourable");

    const minRow = screen.getByRole("rowheader", { name: "Minimum investment" }).closest("tr")!;
    expect(within(minRow).getAllByRole("cell")[1]).toHaveTextContent("+$4,500 vs LOFT");

    const scheduleRow = screen.getByRole("rowheader", { name: "Dividend schedule" }).closest("tr")!;
    const scheduleCells = within(scheduleRow).getAllByRole("cell");
    expect(scheduleCells[0]).toHaveTextContent("Quarterly");
    expect(scheduleCells[1]).toHaveTextContent("Monthly");
    expect(scheduleCells[1]).toHaveAttribute("data-differs", "true");
    expect(scheduleCells[1]).toHaveTextContent("differs from LOFT");
  });

  it("limits the selection to four assets and announces the limit", () => {
    render(<CapTableCompare />);

    fireEvent.click(screen.getByRole("checkbox", { name: /Vaulted Gold/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Solar Farm Notes/ }));

    expect(screen.getByText("Assets to compare (4 of 4 selected)")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /Contemporary Art Fund/ })).toBeDisabled();
    expect(screen.getByText("Maximum of 4 assets selected.")).toBeInTheDocument();
    const headerRow = () => within(screen.getByRole("table")).getAllByRole("row")[0];
    expect(within(headerRow()).getAllByRole("columnheader")).toHaveLength(5);

    fireEvent.click(screen.getByRole("checkbox", { name: /Manhattan Loft/ }));
    expect(screen.getByRole("checkbox", { name: /Contemporary Art Fund/ })).not.toBeDisabled();
    expect(within(headerRow()).getAllByRole("columnheader")[1]).toHaveTextContent("Invoice Pool");
  });

  it("can hide rows where every asset matches", () => {
    render(<CapTableCompare initialSelection={["loft", "art"]} />);
    expect(screen.getByRole("rowheader", { name: "Accredited investors only" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("checkbox", { name: /Show only rows that differ/ }));

    expect(screen.queryByRole("rowheader", { name: "Accredited investors only" })).not.toBeInTheDocument();
    expect(screen.getByRole("rowheader", { name: "Gini coefficient" })).toBeInTheDocument();
  });

  it("exports the comparison as CSV and JSON", () => {
    const onExport = jest.fn();
    const createObjectURL = jest.fn(() => "blob:compare");
    const revokeObjectURL = jest.fn();
    Object.assign(URL, { createObjectURL, revokeObjectURL });
    const click = jest.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});

    render(<CapTableCompare onExport={onExport} />);
    fireEvent.click(screen.getByRole("button", { name: "Export CSV" }));
    fireEvent.click(screen.getByRole("button", { name: "Export JSON" }));

    const [format, csv] = onExport.mock.calls[0];
    expect(format).toBe("csv");
    expect(csv.split("\r\n")[0]).toBe("Metric,Manhattan Loft (LOFT),Invoice Pool 2026-A (INVC)");
    expect(csv).toContain("Gini coefficient,0.380,0.710");
    const json = JSON.parse(onExport.mock.calls[1][1]);
    expect(json.assets.map((a: { symbol: string }) => a.symbol)).toEqual(["LOFT", "INVC"]);
    expect(createObjectURL).toHaveBeenCalledTimes(2);
    expect(click).toHaveBeenCalledTimes(2);
    click.mockRestore();
  });

  it("has no detectable accessibility violations", async () => {
    const { container } = render(<CapTableCompare assets={SAMPLE_COMPARE_ASSETS} initialSelection={["loft", "invc", "gold"]} />);
    expect(await axe(container)).toHaveNoViolations();
  });

  it("loads metrics from the API and refreshes an asset on live events", async () => {
    const fetchMock = jest.fn((url: string) => apiFetch(url));
    global.fetch = fetchMock as unknown as typeof fetch;

    render(<CapTableCompare source="api" apiBaseUrl="https://api.tessera.test" live />);

    expect(screen.getByRole("status")).toHaveTextContent("Loading assets");
    const giniRow = (await screen.findByRole("rowheader", { name: "Gini coefficient" })).closest("tr")!;
    const cells = within(giniRow).getAllByRole("cell");
    expect(cells[0]).toHaveTextContent("0.250");
    // GOLD has no pre-computed Gini, so it is derived from holder balances.
    expect(cells[1]).toHaveTextContent("0.400");

    const minRow = screen.getByRole("rowheader", { name: "Minimum investment" }).closest("tr")!;
    expect(within(minRow).getAllByRole("cell")[0]).toHaveTextContent("$1,000");
    expect(within(minRow).getAllByRole("cell")[0]).toHaveTextContent("price of one token");
    const accreditedRow = screen.getByRole("rowheader", { name: "Accredited investors only" }).closest("tr")!;
    expect(within(accreditedRow).getAllByRole("cell")[0]).toHaveTextContent("Not published");

    await waitFor(() => expect(MockWebSocket.instances.length).toBeGreaterThan(0));
    const socket = MockWebSocket.instances[MockWebSocket.instances.length - 1];
    expect(socket.url).toBe("wss://api.tessera.test/v1/ws");

    jest.useFakeTimers();
    const callsBefore = fetchMock.mock.calls.length;
    act(() => {
      socket.onmessage?.({ data: JSON.stringify({ id: 9, contract: "CLOFT", event_type: "transfer", ledger: 1, data: {} }) });
      socket.onmessage?.({ data: JSON.stringify({ id: 10, contract: "CUNRELATED", event_type: "transfer", ledger: 1, data: {} }) });
    });
    await act(async () => {
      jest.advanceTimersByTime(1_000);
    });
    jest.useRealTimers();
    await waitFor(() => expect(fetchMock.mock.calls.length).toBeGreaterThan(callsBefore));
    const refreshed = fetchMock.mock.calls.slice(callsBefore).map(([url]) => url as string);
    expect(refreshed.every((url) => url.includes("/v1/assets/1/"))).toBe(true);
  });

  it("reports API failures in an alert", async () => {
    global.fetch = jest.fn(() => Promise.resolve({ ok: false, status: 503, json: () => Promise.resolve({}) })) as unknown as typeof fetch;

    render(<CapTableCompare source="api" apiBaseUrl="https://api.tessera.test" />);

    expect(await screen.findByRole("alert")).toHaveTextContent("HTTP 503");
  });
});

describe("loadCompareAsset", () => {
  it("maps API responses into comparison metrics without inventing unpublished values", async () => {
    const loaded = await loadCompareAsset("https://api.tessera.test/", API_ASSETS[0], apiFetch as never);

    expect(loaded).toMatchObject({
      id: "1",
      symbol: "LOFT",
      giniCoefficient: 0.25,
      topHolderSharePercent: 50,
      minInvestmentUsd: 1_000,
      minInvestmentBasis: "one-token",
      annualYieldPercent: 2,
      dividendSchedule: { frequency: "quarterly", distributions: 2 },
      compliance: { jurisdictions: ["US", "DE"], accreditedOnly: null, lockupDays: null, approvedPercent: 75 },
    });
  });
});
