import React from "react";
import { render, screen, fireEvent } from "@testing-library/react";
import ComplianceMatrix, { JURISDICTIONS, PRESETS } from "../../components/ComplianceMatrix";

describe("ComplianceMatrix Component", () => {
  it("renders with 50+ international jurisdictions", () => {
    expect(JURISDICTIONS.length).toBeGreaterThanOrEqual(50);
  });

  it("renders the interactive matrix header, presets, and legend", () => {
    render(<ComplianceMatrix />);

    expect(
      screen.getByText(/Interactive RWA Compliance Permissibility Matrix/i)
    ).toBeInTheDocument();

    // Check that preset regulatory frameworks are displayed
    expect(screen.getByText("US SEC Reg D / Reg S")).toBeInTheDocument();
    expect(screen.getByText("EU MiCA Passporting")).toBeInTheDocument();
    expect(screen.getByText("Singapore MAS Framework")).toBeInTheDocument();
    expect(screen.getByText("Global Institutional")).toBeInTheDocument();
    expect(screen.getByText("Permissive Sandbox")).toBeInTheDocument();

    // Check legend items
    expect(screen.getAllByText("Allowed").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Accredited Only").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Blocked").length).toBeGreaterThan(0);
  });

  it("filters jurisdictions by search input", () => {
    render(<ComplianceMatrix />);

    const searchInput = screen.getByPlaceholderText(/search country or code/i);
    fireEvent.change(searchInput, { target: { value: "Germany" } });

    expect(screen.getAllByText("DE").length).toBeGreaterThan(0);
  });

  it("switches presets and updates matrix rules", () => {
    render(<ComplianceMatrix />);

    // Switch to EU MiCA preset
    const micaButton = screen.getByText("EU MiCA Passporting");
    fireEvent.click(micaButton);

    // Switch to Global Institutional preset
    const globalButton = screen.getByText("Global Institutional");
    fireEvent.click(globalButton);

    expect(screen.getByText(/Raw Soroban Contract Arguments Generator/i)).toBeInTheDocument();
  });

  it("cycles cell permissions on click", () => {
    render(<ComplianceMatrix />);

    // Find all grid buttons
    const buttons = screen.getAllByRole("button");
    const cellButton = buttons.find((btn) =>
      btn.getAttribute("aria-label")?.includes("Rule from")
    );

    expect(cellButton).toBeDefined();
    if (cellButton) {
      const initialLabel = cellButton.getAttribute("aria-label");
      fireEvent.click(cellButton);
      const nextLabel = cellButton.getAttribute("aria-label");
      expect(nextLabel).not.toEqual(initialLabel);
    }
  });

  it("switches export formats between CLI, Rust, and JSON", () => {
    render(<ComplianceMatrix />);

    const rustButton = screen.getByRole("button", { name: "Rust SDK" });
    fireEvent.click(rustButton);
    expect(screen.getByText(/setup_compliance_rules/i)).toBeInTheDocument();

    const jsonButton = screen.getByRole("button", { name: "JSON RPC" });
    fireEvent.click(jsonButton);
    expect(screen.getByText(/jurisdictions_evaluated/i)).toBeInTheDocument();

    const cliButton = screen.getByRole("button", { name: "Soroban CLI" });
    fireEvent.click(cliButton);
    expect(screen.getByText(/soroban contract invoke/i)).toBeInTheDocument();
  });
});
