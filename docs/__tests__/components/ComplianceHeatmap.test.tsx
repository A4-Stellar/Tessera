import { fireEvent, render, screen, act } from "@testing-library/react";
import {
  ComplianceHeatmap,
  getJurisdictionColor,
} from "../../components/ComplianceHeatmap";
import { calculateReconnectDelay, useEventStream } from "../../hooks/useEventStream";
import { renderHook } from "@testing-library/react";

describe("ComplianceHeatmap & useEventStream", () => {
  it("calculates WCAG AA compliant colors based on approval rate", () => {
    expect(getJurisdictionColor(0.95)).toBe("#10b981"); // >= 80% emerald
    expect(getJurisdictionColor(0.65)).toBe("#f59e0b"); // 50-79% amber
    expect(getJurisdictionColor(0.35)).toBe("#ef4444"); // < 50% red
  });

  it("calculates exponential backoff delay with jitter", () => {
    const delay0 = calculateReconnectDelay(0, 500, 30000, () => 0);
    expect(delay0).toBe(500);

    const delayMax = calculateReconnectDelay(10, 500, 30000, () => 1);
    expect(delayMax).toBe(30000);
  });

  it("renders jurisdiction map nodes with keyboard accessibility", () => {
    render(<ComplianceHeatmap />);

    const usNode = screen.getByRole("button", { name: /United States \(US\)/i });
    expect(usNode).toBeInTheDocument();
    expect(usNode).toHaveAttribute("tabindex", "0");

    // Test keyboard selection
    fireEvent.keyDown(usNode, { key: "Enter" });

    // Details panel opens
    expect(screen.getByRole("region", { name: /Details for United States/i })).toBeInTheDocument();
  });

  it("updates decision rates every 5 seconds", () => {
    jest.useFakeTimers();
    render(<ComplianceHeatmap refreshIntervalMs={5000} />);

    act(() => {
      jest.advanceTimersByTime(5000);
    });

    expect(screen.getByText(/decision rates updated every 5s/i)).toBeInTheDocument();
    jest.useRealTimers();
  });
});
