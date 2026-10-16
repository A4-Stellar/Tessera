import { fireEvent, render, screen, within } from "@testing-library/react";
import { axe } from "jest-axe";
import type { AnchorHTMLAttributes } from "react";
import { ExemptionChecker } from "@/components/ExemptionChecker";

// next/link's viewport prefetching schedules state updates outside act() in jsdom.
jest.mock("next/link", () => ({
  __esModule: true,
  default: ({ href, children, ...props }: AnchorHTMLAttributes<HTMLAnchorElement>) => (
    <a href={href} {...props}>
      {children}
    </a>
  ),
}));

const choose = (group: RegExp, option: string) =>
  fireEvent.click(within(screen.getByRole("group", { name: group })).getByLabelText(option));

describe("ExemptionChecker", () => {
  it("walks through the questionnaire, recommends a framework and exports the assessment", async () => {
    const { container } = render(<ExemptionChecker />);
    const next = () => fireEvent.click(screen.getByRole("button", { name: /continue|see results/i }));

    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
    choose(/organized/i, "United States");
    choose(/token give holders/i, "Equity-like interest (shares, LLC or fund units, fractional property ownership)");
    next();

    fireEvent.click(screen.getByLabelText("United States"));
    next();

    choose(/which u\.s\. investors/i, "Accredited investors only");
    choose(/market the offering publicly/i, "Yes");
    fireEvent.change(screen.getByLabelText(/amount to raise/i), { target: { value: "10000000" } });
    choose(/reporting company/i, "No");
    choose(/investment company/i, "No");
    expect(screen.getByRole("button", { name: "See results" })).toBeEnabled();
    next();

    const card = screen.getByRole("listitem", { name: "Regulation D — Rule 506(c)" });
    expect(within(card).getByText("Recommended")).toBeInTheDocument();
    expect(within(card).getAllByRole("link", { name: "Asset token contract reference" })[0]).toHaveAttribute("href", "/docs/contracts/asset-token");
    expect(within(screen.getByRole("listitem", { name: "Regulation D — Rule 506(b)" })).getByText("Not available")).toBeInTheDocument();
    expect(await axe(container)).toHaveNoViolations();

    const blobs: Blob[] = [];
    URL.createObjectURL = jest.fn((blob: Blob) => {
      blobs.push(blob);
      return "blob:assessment";
    });
    URL.revokeObjectURL = jest.fn();
    const click = jest.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    fireEvent.click(screen.getByRole("button", { name: "Export JSON" }));

    expect(click).toHaveBeenCalled();
    const text = await new Promise<string>((resolve) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.readAsText(blobs[0]);
    });
    const report = JSON.parse(text);
    expect(report.answers).toMatchObject({ issuerLocation: "us", usInvestors: "accredited-only", usRaiseUsd: 10_000_000, euInvestors: null });
    expect(report.results.find((result: { id: string }) => result.id === "reg-d-506c").recommended).toBe(true);
  });

  it("asks EU questions only for the EU market and shows MiCA results for crypto-assets", () => {
    render(<ExemptionChecker />);
    const next = () => fireEvent.click(screen.getByRole("button", { name: /continue|see results/i }));

    choose(/organized/i, "European Union");
    choose(/token give holders/i, "Neither — a crypto-asset that is not a security or financial instrument");
    next();
    fireEvent.click(screen.getByLabelText("European Union"));
    next();

    expect(screen.queryByRole("group", { name: /which u\.s\. investors/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("group", { name: /which eu investors/i })).not.toBeInTheDocument();
    choose(/stable value/i, "Yes — other assets, rights or a basket");
    choose(/which eu investors/i, "Includes retail investors");
    choose(/average outstanding value/i, "No");
    next();

    const card = screen.getByRole("listitem", { name: "MiCA Title III — asset-referenced token" });
    expect(within(card).getByText("Required")).toBeInTheDocument();
    expect(within(card).getByRole("link", { name: "MiCA Article 16" })).toHaveAttribute("target", "_blank");
  });
});
