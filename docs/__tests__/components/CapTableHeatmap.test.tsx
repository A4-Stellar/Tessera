import { render, screen, fireEvent, act, within } from "@testing-library/react";
import {
  CapTableHeatmap,
  applyTransfer,
  DEFAULT_HEATMAP_HOLDERS,
  type HeatmapHolder,
  type TransferEvent,
} from "../../components/CapTableHeatmap";

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

describe("CapTableHeatmap", () => {
  beforeEach(() => {
    MockWebSocket.instances = [];
    (global as unknown as { WebSocket: unknown }).WebSocket = MockWebSocket;
  });

  afterEach(() => {
    (global as unknown as { WebSocket: unknown }).WebSocket = originalWebSocket;
  });

  describe("applyTransfer", () => {
    it("moves balance from sender to recipient", () => {
      const holders: HeatmapHolder[] = [
        { address: "GSEND", balance: 100 },
        { address: "GRECV", balance: 50 },
      ];
      const next = applyTransfer(holders, { from: "GSEND", to: "GRECV", amount: 40 });
      expect(next.find((h) => h.address === "GSEND")?.balance).toBe(60);
      expect(next.find((h) => h.address === "GRECV")?.balance).toBe(90);
      expect(holders[0].balance).toBe(100);
    });

    it("appends unknown recipients so new holders appear", () => {
      const holders: HeatmapHolder[] = [{ address: "GSEND", balance: 100 }];
      const next = applyTransfer(holders, { from: "GSEND", to: "GNEW", amount: 25 });
      expect(next).toHaveLength(2);
      expect(next.find((h) => h.address === "GNEW")?.balance).toBe(25);
      expect(next.find((h) => h.address === "GSEND")?.balance).toBe(75);
    });

    it("never lets a balance go negative", () => {
      const holders: HeatmapHolder[] = [{ address: "GSEND", balance: 10 }];
      const next = applyTransfer(holders, { from: "GSEND", to: "GRECV", amount: 50 });
      expect(next.find((h) => h.address === "GSEND")?.balance).toBe(0);
    });

    it("ignores malformed amounts", () => {
      const holders: HeatmapHolder[] = [{ address: "GSEND", balance: 10 }];
      const events: TransferEvent[] = [
        { from: "GSEND", to: "GRECV", amount: "not-a-number" },
        { from: "GSEND", to: "GRECV", amount: -5 },
        { from: "GSEND", to: "GRECV", amount: 0 },
        { from: "GSEND", to: "GRECV" },
      ];
      for (const event of events) {
        expect(applyTransfer(holders, event)).toEqual(holders);
      }
    });

    it("coerces numeric strings", () => {
      const holders: HeatmapHolder[] = [{ address: "GRECV", balance: 5 }];
      const next = applyTransfer(holders, { to: "GRECV", amount: "7" });
      expect(next[0].balance).toBe(12);
    });
  });

  describe("rendering", () => {
    it("renders an accessible heatmap with legend and data table", () => {
      render(<CapTableHeatmap />);
      expect(screen.getByRole("img")).toHaveAttribute(
        "aria-label",
        expect.stringMatching(/8 holders grouped by geographic region/i)
      );
      expect(screen.getByText(/Regions:/)).toBeInTheDocument();
      expect(screen.getByText(/North America \(\d+\.\d%\)/)).toBeInTheDocument();
      const table = screen.getByRole("table");
      expect(within(table).getAllByRole("row")).toHaveLength(9);
      expect(within(table).getByText("Region")).toBeInTheDocument();
    });

    it("colors and groups by compliance status when colorBy=compliance", () => {
      render(<CapTableHeatmap colorBy="compliance" />);
      expect(screen.getByText(/Compliance:/)).toBeInTheDocument();
      const table = screen.getByRole("table");
      expect(within(table).getByText("Compliance")).toBeInTheDocument();
      expect(within(table).getAllByRole("row")).toHaveLength(9);
    });

    it("shows no live badge and opens no socket by default", () => {
      render(<CapTableHeatmap />);
      expect(screen.queryByText(/Live/)).not.toBeInTheDocument();
      expect(MockWebSocket.instances).toHaveLength(0);
    });

    it("opens a websocket and marks the chart live when live is enabled", () => {
      render(<CapTableHeatmap live />);
      expect(screen.getByText(/● Live/)).toBeInTheDocument();
      expect(MockWebSocket.instances).toHaveLength(1);
      expect(MockWebSocket.instances[0].url).toBe("ws://localhost:8080/v1/ws");
    });

    it("applies incoming transfer events to the layout and table", () => {
      render(<CapTableHeatmap live />);
      const socket = MockWebSocket.instances[0];

      act(() => {
        socket.onmessage?.({
          data: JSON.stringify({
            event_type: "contract.transfer",
            data: {
              from: "GALCB4XQM7Q6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXAMPLE",
              to: "GNEWHOLDER000000000000000000000000000000000000",
              amount: 10_000,
            },
          }),
        });
      });

      const table = screen.getByRole("table");
      expect(within(table).getByText("GNEWHO…0000")).toBeInTheDocument();
      expect(within(table).getByText("410,000")).toBeInTheDocument();
      expect(within(table).getAllByRole("row")).toHaveLength(10);
    });

    it("ignores non-transfer events", () => {
      render(<CapTableHeatmap live />);
      const socket = MockWebSocket.instances[0];
      act(() => {
        socket.onmessage?.({
          data: JSON.stringify({
            event_type: "contract.heartbeat",
            data: { from: "GALCB4XQM7Q6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXAMPLE", amount: 10 },
          }),
        });
      });
      expect(within(screen.getByRole("table")).getAllByRole("row")).toHaveLength(9);
    });

    it("accepts an explicit holders snapshot", () => {
      const holders: HeatmapHolder[] = [
        { address: "GSOLO00000000000000000000000000000000000000", balance: 1_000, region: "Europe/EEA" },
      ];
      render(<CapTableHeatmap holders={holders} />);
      expect(screen.getByRole("img")).toHaveAttribute(
        "aria-label",
        expect.stringMatching(/1 holder/)
      );
      expect(within(screen.getByRole("table")).getByText("GSOLO0…0000")).toBeInTheDocument();
    });
  });

  it("exposes DEFAULT_HEATMAP_HOLDERS for docs examples", () => {
    expect(DEFAULT_HEATMAP_HOLDERS.length).toBeGreaterThan(0);
    const total = DEFAULT_HEATMAP_HOLDERS.reduce((sum, holder) => sum + holder.balance, 0);
    expect(total).toBeGreaterThan(0);
  });

  it("updates a balance cell after a manual re-render with edited props", () => {
    const holders: HeatmapHolder[] = [
      { address: "GEDIT000000000000000000000000000000000000000", balance: 100 },
    ];
    const { rerender } = render(<CapTableHeatmap holders={holders} />);
    expect(within(screen.getByRole("table")).getByText("100")).toBeInTheDocument();
    rerender(
      <CapTableHeatmap
        holders={[{ ...holders[0], balance: 250 }]}
      />
    );
    expect(within(screen.getByRole("table")).getByText("250")).toBeInTheDocument();
  });

  it("rerenders when the color selector prop flips", () => {
    const { rerender } = render(<CapTableHeatmap colorBy="region" />);
    expect(screen.getByText(/Regions:/)).toBeInTheDocument();
    rerender(<CapTableHeatmap colorBy="compliance" />);
    expect(screen.getByText(/Compliance:/)).toBeInTheDocument();
  });
});
