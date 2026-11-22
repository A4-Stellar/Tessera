import { render, screen, within } from "@testing-library/react";
import { axe } from "jest-axe";
import { ResourceBudgetVisualiser, ResourceBudgetVisualizer } from "@/components/ResourceBudgetVisualizer";
import type { ResourceUsage } from "@/lib/soroban-resources";
import { buildSorobanWasm } from "../fixtures/sorobanWasm";

const usage = (overrides: Partial<ResourceUsage>): ResourceUsage => ({
  instructions: 0,
  memBytes: 0,
  diskReadEntries: 0,
  diskReadBytes: 0,
  writeEntries: 0,
  writeBytes: 0,
  eventsBytes: 0,
  ...overrides,
});

describe("ResourceBudgetVisualizer", () => {
  it("renders stacked meters and identifies the resource bottleneck", async () => {
    const { container } = render(
      <ResourceBudgetVisualizer
        usage={usage({ instructions: 380_000_000, memBytes: 10_000_000, writeBytes: 100_000 })}
      />
    );

    expect(
      screen.getByText((_, element) => element?.textContent === "Bottleneck: CPU instructions at 95.0% of its limit.")
    ).toBeInTheDocument();
    expect(screen.getByRole("meter", { name: "CPU instructions used" })).toHaveAttribute(
      "aria-valuetext",
      "95.0% of the per-transaction limit"
    );
    expect(screen.getByRole("meter", { name: "Memory used" })).toHaveAttribute("aria-valuetext", "23.8% of the per-transaction limit");
    expect(screen.getByText("Tip: Store compact values and avoid rewriting unchanged state.")).toBeInTheDocument();
    expect(within(screen.getByText("Ledger and storage").parentElement as HTMLElement).getByText("Write bytes")).toBeInTheDocument();
    expect(await axe(container)).toHaveNoViolations();
  });

  it("reports a healthy budget when all resources are below thresholds", () => {
    render(<ResourceBudgetVisualizer usage={usage({ instructions: 1, memBytes: 1 })} />);

    expect(screen.getByText("All resources are below the recommended thresholds.")).toBeInTheDocument();
    expect(screen.queryByText(/Tip:/)).not.toBeInTheDocument();
  });

  it("parses the soroban_budget custom section and renders a stacked bar chart of instruction counts by category", async () => {
    const wasmWithBudget = buildSorobanWasm({
      budget: {
        compute: 20_000_000,
        memory: 10_000_000,
        storage: 15_000_000,
        host_calls: 25_000_000,
        crypto: 10_000_000,
        mem_bytes: 4_194_304,
      },
    });

    const { container } = render(<ResourceBudgetVisualiser wasmBytes={wasmWithBudget} />);

    const chartRegion = screen.getByRole("region", {
      name: /instruction counts by category vs\. soroban budget/i,
    });
    expect(chartRegion).toBeInTheDocument();
    expect(within(chartRegion).getByText("soroban_budget custom section")).toBeInTheDocument();
    expect(within(chartRegion).getByText("Compute & arithmetic")).toBeInTheDocument();
    expect(within(chartRegion).getByText("20,000,000 instrs")).toBeInTheDocument();
    expect(within(chartRegion).getByText("Host invocations")).toBeInTheDocument();
    expect(within(chartRegion).getByText("25,000,000 instrs")).toBeInTheDocument();
    expect(await axe(container)).toHaveNoViolations();
  });
});
