import { render, screen, fireEvent, within } from "@testing-library/react";
import {
  WaterfallVisualizer,
  computeWaterfall,
  serializeChart,
  DEFAULT_TRANCHES,
  type WaterfallTranche,
} from "../../components/WaterfallVisualizer";

describe("WaterfallVisualizer", () => {
  const originalCreateObjectUrl = URL.createObjectURL;
  const originalRevokeObjectUrl = URL.revokeObjectURL;

  beforeAll(() => {
    URL.createObjectURL = jest.fn(() => "blob:waterfall-test");
    URL.revokeObjectURL = jest.fn();
    jest
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(function (this: HTMLAnchorElement) {
        /* download anchor should not navigate in jsdom */
      });
  });

  afterAll(() => {
    URL.createObjectURL = originalCreateObjectUrl;
    URL.revokeObjectURL = originalRevokeObjectUrl;
    jest.restoreAllMocks();
  });

  describe("computeWaterfall", () => {
    it("pays tranches strictly in priority order", () => {
      const result = computeWaterfall(9_000_000, DEFAULT_TRANCHES);
      const ids = result.steps.map((step) => step.id);
      expect(ids).toEqual([
        "proceeds",
        "senior",
        "mezz",
        "pref",
        "common",
        "residual",
      ]);
      expect(result.steps.find((s) => s.id === "senior")?.paid).toBe(4_000_000);
      expect(result.steps.find((s) => s.id === "mezz")?.paid).toBe(1_500_000);
      expect(result.steps.find((s) => s.id === "pref")?.paid).toBe(2_500_000);
      expect(result.steps.find((s) => s.id === "common")?.paid).toBe(1_000_000);
    });

    it("allocates shortfall to lower-priority tranches when proceeds are insufficient", () => {
      const result = computeWaterfall(3_000_000, DEFAULT_TRANCHES);
      const senior = result.steps.find((s) => s.id === "senior")!;
      const mezz = result.steps.find((s) => s.id === "mezz")!;
      const common = result.steps.find((s) => s.id === "common")!;

      expect(senior.paid).toBe(3_000_000);
      expect(senior.shortfall).toBe(1_000_000);
      expect(mezz.paid).toBe(0);
      expect(mezz.shortfall).toBe(1_500_000);
      expect(common.paid).toBe(0);
      expect(common.shortfall).toBe(5_000_000);
      expect(result.residual).toBe(0);
      expect(result.totalShortfall).toBe(
        1_000_000 + 1_500_000 + 2_500_000 + 5_000_000
      );
    });

    it("keeps ordering when tranche priorities are supplied out of order", () => {
      const shuffled: WaterfallTranche[] = [
        { id: "b", label: "B", claim: 100, priority: 2, kind: "mezz-debt" },
        { id: "a", label: "A", claim: 100, priority: 1, kind: "senior-debt" },
      ];
      const result = computeWaterfall(150, shuffled);
      expect(result.steps.map((s) => s.id)).toEqual(["proceeds", "a", "b", "residual"]);
      expect(result.steps[1].paid).toBe(100);
      expect(result.steps[2].paid).toBe(50);
      expect(result.steps[2].shortfall).toBe(50);
    });

    it("pays everything through and leaves no residual when proceeds exceed claims", () => {
      const result = computeWaterfall(20_000_000, DEFAULT_TRANCHES);
      expect(result.totalShortfall).toBe(0);
      expect(result.residual).toBe(7_000_000);
    });
  });

  describe("UI", () => {
    it("renders an accessible chart, legend, inputs and summary", () => {
      render(<WaterfallVisualizer />);
      expect(
        screen.getByRole("img", { name: /Waterfall chart of capital distribution/i })
      ).toBeInTheDocument();
      expect(screen.getByLabelText("Exit proceeds in dollars")).toBeInTheDocument();
      expect(screen.getByRole("table", { name: /distribution summary/i })).toBeInTheDocument();
      expect(screen.getAllByText("Shortfall").length).toBeGreaterThanOrEqual(2);
      expect(screen.getByRole("button", { name: "Export SVG" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Export PNG" })).toBeInTheDocument();
    });

    it("recalculates shortfall when the exit valuation changes", () => {
      render(<WaterfallVisualizer />);
      const summary = screen.getByRole("table", { name: /distribution summary/i });
      expect(within(summary).getAllByText("$4,000,000")).toHaveLength(2);

      fireEvent.change(screen.getByLabelText("Exit proceeds in dollars"), {
        target: { value: "3000000" },
      });

      const updated = screen.getByRole("table", { name: /distribution summary/i });
      expect(within(updated).getByText("$1,000,000")).toBeInTheDocument();
      expect(within(updated).getByText("$1,500,000")).toBeInTheDocument();
      expect(within(updated).getByText("$5,000,000")).toBeInTheDocument();
    });

    it("updates summary when a tranche claim is edited", () => {
      render(<WaterfallVisualizer />);
      const seniorClaim = screen.getByLabelText(/Claim for Senior Secured Debt/i);
      fireEvent.change(seniorClaim, { target: { value: "1000000" } });
      const summary = screen.getByRole("table", { name: /distribution summary/i });
      const seniorRow = within(summary)
        .getByText("Senior Secured Debt")
        .closest("tr")!;
      expect(within(seniorRow).getByText("$1,000,000")).toBeInTheDocument();
      expect(within(seniorRow).getByText("—")).toBeInTheDocument();
    });

    it("adds and removes tranches", () => {
      render(<WaterfallVisualizer />);
      fireEvent.click(screen.getByRole("button", { name: "+ Add tranche" }));
      expect(screen.getAllByText("Custom Tranche 5").length).toBeGreaterThan(0);

      fireEvent.click(screen.getByRole("button", { name: "Remove Custom Tranche 5" }));
      expect(screen.queryByText("Custom Tranche 5")).not.toBeInTheDocument();
    });

    it("exports the chart as SVG and reports status", async () => {
      render(<WaterfallVisualizer />);
      fireEvent.click(screen.getByRole("button", { name: "Export SVG" }));
      expect(await screen.findByRole("status")).toHaveTextContent("SVG exported.");
      expect(URL.createObjectURL).toHaveBeenCalled();
    });

    it("starts PNG rasterization when Export PNG is clicked", () => {
      render(<WaterfallVisualizer />);
      fireEvent.click(screen.getByRole("button", { name: "Export PNG" }));
      expect(screen.getByRole("status")).toHaveTextContent("Rasterizing PNG");
    });

    it("serializes the live SVG element for export", () => {
      const { container } = render(<WaterfallVisualizer />);
      const svg = container.querySelector("svg")!;
      const markup = serializeChart(svg as SVGSVGElement);
      expect(markup).toContain("<?xml version");
      expect(markup).toContain("<svg");
      expect(markup).toContain("xmlns=");
    });
  });
});
