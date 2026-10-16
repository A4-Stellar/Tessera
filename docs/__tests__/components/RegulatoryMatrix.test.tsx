import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import RegulatoryMatrix, { REGULATORY_RULES, ruleArguments } from "@/components/RegulatoryMatrix";

describe("RegulatoryMatrix", () => {
  it("compares the four supported frameworks and produces contract arguments", () => {
    render(<RegulatoryMatrix />);

    expect(REGULATORY_RULES).toHaveLength(4);
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getAllByText("SEC Regulation D")).toHaveLength(2);
    expect(screen.getAllByText("EU MiCA")).toHaveLength(2);
    expect(ruleArguments(REGULATORY_RULES).lockup_days["reg-s"]).toBe(40);
  });

  it("updates the matrix and arguments when a rule is deselected", () => {
    render(<RegulatoryMatrix />);

    fireEvent.click(screen.getByRole("checkbox", { name: "SEC Regulation D" }));

    expect(screen.getAllByText("SEC Regulation D")).toHaveLength(1);
    expect(screen.getByText(/"reg-s"/)).toBeInTheDocument();
    expect(screen.queryByText(/"reg-d"/)).not.toBeInTheDocument();
  });

  it("copies selected Soroban initialization arguments", async () => {
    const writeText = jest.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(<RegulatoryMatrix />);

    fireEvent.click(screen.getByRole("button", { name: "Copy Soroban initialization arguments" }));

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(expect.stringContaining("regulatory_frameworks")),
    );
  });
});
