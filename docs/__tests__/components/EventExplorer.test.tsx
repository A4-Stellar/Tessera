import { render, screen, fireEvent, act } from "@testing-library/react";
import { EventExplorer } from "../../components/EventExplorer";
import { SAMPLE_SOROBAN_EVENTS, decodeSorobanXdr } from "../../lib/xdr-decoder";

describe("EventExplorer Component", () => {
  beforeEach(() => {
    jest.clearAllMocks();
    Object.assign(navigator, {
      clipboard: {
        writeText: jest.fn().mockImplementation(() => Promise.resolve()),
      },
    });
  });

  it("renders with sample preset loaded by default", () => {
    render(<EventExplorer />);

    expect(screen.getByText("Soroban Event Explorer & XDR Decoder")).toBeInTheDocument();
    expect(screen.getByText(/Sample presets:/i)).toBeInTheDocument();
    expect(screen.getByText("Decoded Events:")).toBeInTheDocument();
    expect(screen.getByText("Event #1")).toBeInTheDocument();
  });

  it("decodes raw base64 ContractEvent successfully", () => {
    const result = decodeSorobanXdr(SAMPLE_SOROBAN_EVENTS[0].xdr);
    expect(result.success).toBe(true);
    expect(result.events.length).toBeGreaterThan(0);
    expect(result.events[0].topics.length).toBeGreaterThan(0);
  });

  it("switches between sample presets when clicking preset buttons", () => {
    render(<EventExplorer />);

    const complianceBtn = screen.getByRole("button", { name: /Load sample: Compliance Allowlist Add/i });
    fireEvent.click(complianceBtn);

    expect(screen.getByText("Event #1")).toBeInTheDocument();
    expect(screen.getAllByText(/allowlist_add/i).length).toBeGreaterThan(0);
  });

  it("collapses and expands event tree cards", () => {
    render(<EventExplorer />);

    const toggleBtn = screen.getByRole("button", { name: /Event #1/i });
    expect(toggleBtn).toHaveAttribute("aria-expanded", "true");

    // Collapse
    fireEvent.click(toggleBtn);
    expect(toggleBtn).toHaveAttribute("aria-expanded", "false");

    // Expand again
    fireEvent.click(toggleBtn);
    expect(toggleBtn).toHaveAttribute("aria-expanded", "true");
  });

  it("shows error alert on invalid XDR input", async () => {
    render(<EventExplorer />);

    const input = screen.getByLabelText(/Base64 Soroban XDR/i);
    fireEvent.change(input, { target: { value: "invalid_not_real_xdr_!!!" } });

    const decodeBtn = screen.getByRole("button", { name: /Decode & Inspect Events/i });
    await act(async () => {
      fireEvent.click(decodeBtn);
    });

    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByText(/Unable to decode XDR/i)).toBeInTheDocument();
  });

  it("copies full JSON representation when clicking Copy Full JSON", async () => {
    render(<EventExplorer />);

    const copyBtn = screen.getByRole("button", { name: /Copy full decoded JSON representation/i });
    await act(async () => {
      fireEvent.click(copyBtn);
    });

    expect(navigator.clipboard.writeText).toHaveBeenCalled();
    expect(screen.getByText("✓ JSON Copied!")).toBeInTheDocument();
  });
});
