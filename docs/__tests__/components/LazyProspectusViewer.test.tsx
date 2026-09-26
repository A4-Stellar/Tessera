import { act, render, screen } from "@testing-library/react";
import { LazyProspectusViewer } from "../../components/LazyProspectusViewer";

jest.mock("../../components/ProspectusViewer", () => ({
  __esModule: true,
  ProspectusViewer: ({ title }: { title?: string }) => <div data-testid="viewer">{title}</div>,
}));

describe("LazyProspectusViewer", () => {
  const original = globalThis.IntersectionObserver;
  afterEach(() => {
    globalThis.IntersectionObserver = original;
  });

  it("does not mount the viewer until intersecting", async () => {
    let callback: IntersectionObserverCallback = () => {};
    globalThis.IntersectionObserver = jest.fn((cb: IntersectionObserverCallback) => {
      callback = cb;
      return { observe: jest.fn(), disconnect: jest.fn(), unobserve: jest.fn() };
    }) as unknown as typeof IntersectionObserver;

    render(<LazyProspectusViewer src="https://example.com/a.pdf" title="Audit" />);
    expect(screen.queryByTestId("viewer")).toBeNull();

    await act(async () => {
      callback([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver);
    });
    expect(await screen.findByTestId("viewer")).toHaveTextContent("Audit");
  });

  it("mounts immediately without IntersectionObserver", async () => {
    // @ts-expect-error simulate unsupported browser
    delete globalThis.IntersectionObserver;
    render(<LazyProspectusViewer src="https://example.com/a.pdf" title="Audit" />);
    expect(await screen.findByTestId("viewer")).toBeInTheDocument();
  });
});
