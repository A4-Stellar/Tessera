import React from "react";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { axe } from "jest-axe";
import { WasmSandbox } from "@/components/WasmSandbox";

describe("WasmSandbox Component", () => {
  it("renders the sandbox interface with header and default preset", () => {
    render(<WasmSandbox defaultContract="asset-token" />);
    expect(screen.getByTestId("wasm-sandbox")).toBeInTheDocument();
    expect(screen.getByText("Soroban Rust WASM Sandbox")).toBeInTheDocument();
    expect(screen.getByText(/WASI WebWorker Runtime/i)).toBeInTheDocument();
    expect(screen.getByText(/Tessera RWA Asset Token/i)).toBeInTheDocument();
  });

  it("switches contract presets and updates available functions", () => {
    render(<WasmSandbox defaultContract="asset-token" />);
    const select = screen.getByLabelText(/contract preset selector/i);
    fireEvent.change(select, { target: { value: "compliance" } });

    expect(screen.getByText(/Jurisdiction Compliance Gate/i)).toBeInTheDocument();
    expect(screen.getByText(/is_allowed\(\)/i)).toBeInTheDocument();
    expect(screen.getByText(/set_status\(\)/i)).toBeInTheDocument();
  });

  it("executes a function and displays raw WASI logs, return data, and gas metrics", async () => {
    render(<WasmSandbox defaultContract="asset-token" />);

    // Click balance function
    const balanceBtn = screen.getByText(/balance\(\)/i);
    fireEvent.click(balanceBtn);

    // Click execute
    const executeBtn = screen.getByRole("button", { name: /execute balance/i });
    fireEvent.click(executeBtn);

    // Wait for execution completion and logs
    await waitFor(() => {
      expect(screen.getByRole("region", { name: /wasm logs/i })).toBeInTheDocument();
      expect(screen.getByText(/Contract query balance/i)).toBeInTheDocument();
    });

    // Check Return Data tab
    const returnTab = screen.getByRole("button", { name: /return data/i });
    fireEvent.click(returnTab);
    expect(screen.getByRole("region", { name: /returned data structure/i })).toBeInTheDocument();
    expect(screen.getByText(/returned successfully/i)).toBeInTheDocument();

    // Check Gas & Metrics tab
    const metricsTab = screen.getByRole("button", { name: /gas & metrics/i });
    fireEvent.click(metricsTab);
    expect(screen.getByRole("region", { name: /gas and resource metrics/i })).toBeInTheDocument();
    expect(screen.getByText(/CPU Instructions/i)).toBeInTheDocument();
    expect(screen.getByText(/Memory Footprint/i)).toBeInTheDocument();
    expect(screen.getByText(/Storage Ops/i)).toBeInTheDocument();
  });

  it("inspects and mutates simulated ledger state in IndexedDB tab", async () => {
    render(<WasmSandbox defaultContract="asset-token" />);

    const storageTab = screen.getByRole("button", { name: /indexeddb state/i });
    fireEvent.click(storageTab);

    expect(screen.getByRole("region", { name: /simulated state storage/i })).toBeInTheDocument();
    expect(screen.getByRole("table", { name: /storage entries table/i })).toBeInTheDocument();
    expect(screen.getByText("TOKEN_DECIMALS")).toBeInTheDocument();

    // Add a custom key-value
    const keyInput = screen.getByPlaceholderText(/storage key/i);
    const valueInput = screen.getByPlaceholderText(/^value$/i);
    const saveBtn = screen.getByRole("button", { name: /^save$/i });

    fireEvent.change(keyInput, { target: { value: "CUSTOM_FLAG" } });
    fireEvent.change(valueInput, { target: { value: "ACTIVE_VAL" } });
    fireEvent.click(saveBtn);

    await waitFor(() => {
      expect(screen.getByText("CUSTOM_FLAG")).toBeInTheDocument();
      expect(screen.getByText("ACTIVE_VAL")).toBeInTheDocument();
    });
  });

  it("passes axe accessibility checks", async () => {
    const { container } = render(<WasmSandbox defaultContract="asset-token" />);
    const results = await axe(container);
    expect(results).toHaveNoViolations();
  });
});
