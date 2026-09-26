import { render, screen, fireEvent } from "@testing-library/react";

const push = jest.fn();
jest.mock("next/navigation", () => ({
  useRouter: () => ({ push }),
}));

jest.mock("../../lib/search", () => ({
  initSearch: jest.fn().mockResolvedValue(undefined),
  search: (query: string) => {
    const normalized = query.toLowerCase();
    return normalized === "api" || normalized === "compliance"
      ? [{ id: 1, route: "/docs/compliance", title: "API compliance", headers: "", content: "" }]
      : [];
  },
}));

import { Search } from "../../components/Search";

describe("Search", () => {
  it("shows an empty state when a query has zero results", async () => {
    render(<Search />);
    const input = screen.getByPlaceholderText(/search docs/i);

    fireEvent.change(input, { target: { value: "zzzznotarealresult" } });

    expect(await screen.findByText(/no results for/i)).toBeInTheDocument();
  });

  it("does not show an empty state before typing", () => {
    render(<Search />);
    expect(screen.queryByText(/no results for/i)).not.toBeInTheDocument();
  });

  it("shows matching results for a valid query", async () => {
    render(<Search />);
    const input = screen.getByPlaceholderText(/search docs/i);

    fireEvent.change(input, { target: { value: "compliance" } });

    expect((await screen.findAllByText(/compliance/i)).length).toBeGreaterThan(0);
    expect(screen.queryByText(/no results for/i)).not.toBeInTheDocument();
  });

  it("moves the active selection with ArrowDown/ArrowUp", async () => {
    render(<Search />);
    const input = screen.getByPlaceholderText(/search docs/i);

    fireEvent.change(input, { target: { value: "api" } });
    const options = await screen.findAllByRole("option");
    fireEvent.keyDown(input, { key: "ArrowDown" });

    expect(options[0]).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(options[options.length - 1]).toHaveAttribute("aria-selected", "true");
  });

  it("navigates to the active result on Enter", async () => {
    render(<Search />);
    const input = screen.getByPlaceholderText(/search docs/i);

    fireEvent.change(input, { target: { value: "api" } });
    await screen.findAllByRole("option");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(push).toHaveBeenCalled();
  });

  it("closes the results on Escape", async () => {
    render(<Search />);
    const input = screen.getByPlaceholderText(/search docs/i);

    fireEvent.change(input, { target: { value: "api" } });
    expect((await screen.findAllByRole("option")).length).toBeGreaterThan(0);

    fireEvent.keyDown(input, { key: "Escape" });

    expect(screen.queryAllByRole("option").length).toBe(0);
  });
});
