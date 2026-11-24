import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import TokenEconomicsSimulator from "@/components/TokenEconomicsSimulator";

describe("TokenEconomicsSimulator", () => {
  it("renders sliders, runs in-process without a Worker and draws the outputs", async () => {
    const { container } = render(
      <TokenEconomicsSimulator initialParams={{ steps: 60 }} name="Test scenario" />,
    );
    expect(screen.getByTestId("token-economics-simulator")).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: /Institutional holders/ })).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: /Retail holders/ })).toBeInTheDocument();
    expect(screen.getByRole("slider", { name: /Arbitrageurs/ })).toBeInTheDocument();

    await waitFor(() => expect(screen.getByTestId("simulator-engine")).toHaveTextContent("reference"));
    expect(screen.getByRole("img", { name: /AMM price series/ })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: /Holder distribution over time/ })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: /Liquidity depth/ })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: /Dividend yield/ })).toBeInTheDocument();
    expect(screen.getByText("Final Gini")).toBeInTheDocument();
    expect(container.querySelector('[role="progressbar"]')).toHaveAttribute("aria-valuenow", "100");
  });

  it("re-runs when a slider changes", async () => {
    render(<TokenEconomicsSimulator initialParams={{ steps: 40 }} />);
    await waitFor(() => expect(screen.getByTestId("simulator-engine")).toHaveTextContent("reference"));
    const slider = screen.getByRole("slider", { name: /Arbitrageurs/ });
    fireEvent.change(slider, { target: { value: "80" } });
    await waitFor(() => expect(screen.getByRole("img", { name: /AMM price series/ })).toBeInTheDocument());
  });
});
