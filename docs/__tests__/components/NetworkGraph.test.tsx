import React from "react";
import { render, screen, fireEvent, within } from "@testing-library/react";
import { axe } from "jest-axe";
import { NetworkGraph } from "@/components/NetworkGraph";

describe("NetworkGraph Component", () => {
  beforeAll(() => {
    // Mock ResizeObserver for jsdom
    global.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  });

  it("renders the network graph container and toolbar stats", () => {
    render(<NetworkGraph initialNodeCount={500} />);
    expect(screen.getByTestId("network-graph")).toBeInTheDocument();
    expect(screen.getByText("Financial Network Topology")).toBeInTheDocument();
    expect(screen.getByText(/500 nodes ·/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /jurisdiction clusters/i })).toBeInTheDocument();
  });

  it("toggles clustering mode between jurisdiction clusters and free force layout", () => {
    render(<NetworkGraph initialNodeCount={500} defaultClustered={true} />);
    const clusterBtn = screen.getByRole("button", { name: /jurisdiction clusters: on/i });
    expect(clusterBtn).toBeInTheDocument();

    fireEvent.click(clusterBtn);
    expect(screen.getByRole("button", { name: /free force layout/i })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /free force layout/i }));
    expect(screen.getByRole("button", { name: /jurisdiction clusters: on/i })).toBeInTheDocument();
  });

  it("allows changing node scale up to 10,000 nodes", () => {
    render(<NetworkGraph initialNodeCount={500} />);
    const select = screen.getByLabelText(/node count scale/i);
    expect(select).toBeInTheDocument();

    fireEvent.change(select, { target: { value: "10000" } });
    expect(screen.getByText(/10,000 nodes ·/i)).toBeInTheDocument();
  });

  it("toggles the accessible data table and displays node metadata", () => {
    render(<NetworkGraph initialNodeCount={500} />);
    const tableBtn = screen.getByRole("button", { name: /data table/i });
    fireEvent.click(tableBtn);

    expect(screen.getByText(/Network Node Registry/i)).toBeInTheDocument();
    expect(screen.getByRole("table", { name: /network nodes table/i })).toBeInTheDocument();

    // Inspect first node in table
    const inspectButtons = screen.getAllByRole("button", { name: /inspect/i });
    expect(inspectButtons.length).toBeGreaterThan(0);
    fireEvent.click(inspectButtons[0]);

    // Inspector region should appear
    const inspector = screen.getByRole("region", { name: /node metadata inspector/i });
    expect(inspector).toBeInTheDocument();
    expect(within(inspector).getByText(/Holding Balance/i)).toBeInTheDocument();
    expect(within(inspector).getByText(/^Jurisdiction$/i)).toBeInTheDocument();
    expect(within(inspector).getByText(/^Compliance$/i)).toBeInTheDocument();
  });

  it("filters nodes by type pill", () => {
    render(<NetworkGraph initialNodeCount={500} />);
    const tableBtn = screen.getByRole("button", { name: /data table/i });
    fireEvent.click(tableBtn);

    const assetFilterBtn = screen.getByRole("button", { name: /^asset$/i });
    fireEvent.click(assetFilterBtn);

    const cells = screen.getAllByRole("cell");
    expect(cells.length).toBeGreaterThan(0);
  });

  it("passes axe accessibility checks", async () => {
    const { container } = render(<NetworkGraph initialNodeCount={500} />);
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });
});
