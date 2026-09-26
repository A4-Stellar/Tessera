import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  isSafePdfSource,
  normalizeSha256,
  ProspectusViewer,
  sha256Hex,
} from "../../components/ProspectusViewer";

const mockGetDocument = jest.fn();
const mockDigest = jest.fn();

jest.mock("pdfjs-dist", () => ({
  __esModule: true,
  GlobalWorkerOptions: { workerSrc: "" },
  version: "4.10.38",
  getDocument: (...args: unknown[]) => mockGetDocument(...args),
}));

type MockPage = {
  getViewport: jest.Mock;
  render: jest.Mock;
  getTextContent: jest.Mock;
};

type MockPdf = {
  numPages: number;
  getPage: jest.Mock;
  destroy: jest.Mock;
};

function createPdf() {
  const page: MockPage = {
    getViewport: jest.fn(({ scale }: { scale: number }) => ({ width: 100 * scale, height: 120 * scale })),
    render: jest.fn(() => ({ promise: Promise.resolve(), cancel: jest.fn() })),
    getTextContent: jest.fn().mockResolvedValue({ items: [{ str: "Alpha alpha" }] }),
  };
  const pdf: MockPdf = {
    numPages: 2,
    getPage: jest.fn().mockResolvedValue(page),
    destroy: jest.fn().mockResolvedValue(undefined),
  };
  const task = { promise: Promise.resolve(pdf), destroy: jest.fn().mockResolvedValue(undefined) };
  mockGetDocument.mockReturnValue(task);
  return { page, pdf, task };
}

function pdfResponse(bytes: Uint8Array): Response {
  return {
    ok: true,
    status: 200,
    arrayBuffer: jest.fn().mockResolvedValue(bytes.slice().buffer),
  } as unknown as Response;
}

describe("ProspectusViewer", () => {
  const originalFetch = globalThis.fetch;
  const originalGetContext = HTMLCanvasElement.prototype.getContext;
  const originalCrypto = Object.getOwnPropertyDescriptor(globalThis, "crypto");

  beforeEach(() => {
    jest.clearAllMocks();
    globalThis.fetch = jest.fn() as unknown as typeof fetch;
    Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
      configurable: true,
      value: jest.fn(() => ({}) as unknown as CanvasRenderingContext2D),
    });
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
      configurable: true,
      value: originalGetContext,
    });
    if (originalCrypto) {
      Object.defineProperty(globalThis, "crypto", originalCrypto);
    } else {
      delete (globalThis as { crypto?: Crypto }).crypto;
    }
  });

  it("normalizes supported on-chain hash formats", () => {
    const hash = "01".repeat(32);

    expect(normalizeSha256(`sha256:${hash}`)).toBe(hash);
    expect(normalizeSha256(`0x${hash}`)).toBe(hash);
    expect(normalizeSha256("not-a-hash")).toBeNull();
    expect(isSafePdfSource("https://example.com/prospectus.pdf")).toBe(true);
    expect(isSafePdfSource("http://localhost:3000/prospectus.pdf")).toBe(true);
    expect(isSafePdfSource("http://example.com/prospectus.pdf")).toBe(false);
    expect(isSafePdfSource("javascript:alert(1)")).toBe(false);
  });

  it("loads a PDF and exposes page, search, zoom, and swipe controls", async () => {
    const { page, pdf } = createPdf();
    (globalThis.fetch as jest.Mock).mockResolvedValue(pdfResponse(new Uint8Array([1, 2, 3])));

    render(<ProspectusViewer src="https://example.com/prospectus.pdf" />);

    expect(screen.getByText("Loading prospectus…")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("of 2")).toBeInTheDocument());
    expect(mockGetDocument).toHaveBeenCalledWith(
      expect.objectContaining({ isEvalSupported: false })
    );
    expect(screen.getByLabelText("Rendered PDF page 1 of 2")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await waitFor(() => expect(screen.getByLabelText("Current PDF page")).toHaveValue(2));
    expect(pdf.getPage).toHaveBeenCalledWith(2);

    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    expect(screen.getByLabelText("Zoom level")).toHaveTextContent("110%");

    fireEvent.change(screen.getByLabelText("Search prospectus text"), {
      target: { value: "alpha" },
    });
    fireEvent.submit(screen.getByRole("search"));
    await waitFor(() => expect(screen.getByText("1 of 4 matches")).toBeInTheDocument());
    expect(page.getTextContent).toHaveBeenCalled();

    const viewport = screen.getByTestId("prospectus-page-viewport");
    fireEvent.touchStart(viewport, {
      touches: [{ clientX: 200, clientY: 20 }],
      changedTouches: [{ clientX: 200, clientY: 20 }],
    });
    fireEvent.touchEnd(viewport, {
      touches: [],
      changedTouches: [{ clientX: 100, clientY: 20 }],
    });
    await waitFor(() => expect(screen.getByLabelText("Current PDF page")).toHaveValue(2));
  });

  it("reports a SHA-256 mismatch without evaluating the file as code", async () => {
    const digest = new Uint8Array(32).fill(1).buffer;
    mockDigest.mockResolvedValue(digest);
    Object.defineProperty(globalThis, "crypto", {
      configurable: true,
      value: { subtle: { digest: mockDigest } },
    });
    createPdf();
    (globalThis.fetch as jest.Mock).mockResolvedValue(pdfResponse(new Uint8Array([1, 2, 3])));

    render(
      <ProspectusViewer
        src="https://example.com/prospectus.pdf"
        expectedHash={"02".repeat(32)}
      />
    );

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(/verification failed/i));
    expect(screen.getByText("of 2")).toBeInTheDocument();
  });

  it("calculates a digest through the Web Crypto API", async () => {
    mockDigest.mockResolvedValue(new Uint8Array(32).fill(7).buffer);
    Object.defineProperty(globalThis, "crypto", {
      configurable: true,
      value: { subtle: { digest: mockDigest } },
    });

    await expect(sha256Hex(new Uint8Array([1]))).resolves.toBe("07".repeat(32));
    expect(mockDigest).toHaveBeenCalled();
  });
});
