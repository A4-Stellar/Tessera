import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  isSecureEndpoint,
  normalizeCompilerResponse,
  SorobanIDE,
} from "../../components/SorobanIDE";

type MockEditorProps = {
  value?: string;
  onChange?: (value: string | undefined) => void;
  language?: string;
};

jest.mock("next/dynamic", () => ({
  __esModule: true,
  default: (loader: unknown) => {
    void loader;
    const React = jest.requireActual("react") as typeof import("react");
    return function MockEditor({ value, onChange, language }: MockEditorProps) {
      return React.createElement("textarea", {
        "aria-label": "Rust source editor",
        "data-language": language,
        value: value || "",
        onChange: (event: React.ChangeEvent<HTMLTextAreaElement>) => onChange?.(event.target.value),
      });
    };
  },
}));

const TESTNET_PASSPHRASE = "Test SDF Network ; September 2015";
const compilerEndpoint = "https://sandbox.example.test/compile";

function jsonResponse(payload: unknown): Response {
  return {
    ok: true,
    status: 200,
    text: jest.fn().mockResolvedValue(JSON.stringify(payload)),
    json: jest.fn().mockResolvedValue(payload),
  } as unknown as Response;
}

describe("SorobanIDE", () => {
  const originalFetch = globalThis.fetch;
  const freighter = {
    getAddress: jest.fn(),
    getNetwork: jest.fn(),
    signTransaction: jest.fn(),
  };

  beforeEach(() => {
    jest.clearAllMocks();
    globalThis.fetch = jest.fn() as unknown as typeof fetch;
    const browserWindow = window as Window & { freighter?: unknown };
    browserWindow.freighter = freighter;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    const browserWindow = window as Window & { freighter?: unknown };
    delete browserWindow.freighter;
  });

  it("validates compiler endpoint security", () => {
    expect(isSecureEndpoint("https://sandbox.example.test/compile")).toBe(true);
    expect(isSecureEndpoint("http://localhost:4000/compile")).toBe(true);
    expect(isSecureEndpoint("http://sandbox.example.test/compile")).toBe(false);
    expect(isSecureEndpoint("javascript:alert(1)")).toBe(false);
  });

  it("normalizes compiler artifacts and rejects invalid responses", () => {
    expect(normalizeCompilerResponse({ data: { wasm: "AGFzbQ==" } })).toEqual({
      wasm: "AGFzbQ==",
      warnings: [],
      logs: [],
      contractId: undefined,
    });
    expect(() => normalizeCompilerResponse({ success: false, error: "compile failed" })).toThrow(
      "compile failed"
    );
    expect(() => normalizeCompilerResponse({ error: "compile failed", data: { wasm: "AGFzbQ==" } })).toThrow(
      "compile failed"
    );
    expect(() => normalizeCompilerResponse({ wasm: "AAAA" })).toThrow(/WASM artifact/);
    expect(() => normalizeCompilerResponse({ result: {} })).toThrow(/WASM or a deployment transaction/);
  });

  it("compiles through the configured sandbox and signs a Testnet deployment", async () => {
    (globalThis.fetch as jest.Mock).mockImplementation((input: RequestInfo | URL) => {
      if (String(input) === compilerEndpoint) {
        return Promise.resolve(
          jsonResponse({
            transactionXdr: "AAAA",
            warnings: ["review the contract"],
            logs: ["compiled"],
          })
        );
      }
      return Promise.resolve(
        jsonResponse({ jsonrpc: "2.0", result: { hash: "tx-hash", status: "PENDING" } })
      );
    });
    freighter.getAddress.mockResolvedValue({ address: "GABC" });
    freighter.getNetwork.mockResolvedValue({
      network: "TESTNET",
      networkPassphrase: TESTNET_PASSPHRASE,
    });
    freighter.signTransaction.mockResolvedValue({ signedTxXdr: "SIGNED-XDR" });

    render(<SorobanIDE compilerEndpoint={compilerEndpoint} initialCode="fn main() {}" />);

    expect(screen.getByRole("textbox", { name: "Rust source editor" })).toHaveValue("fn main() {}");
    fireEvent.click(screen.getByRole("button", { name: "Compile Rust" }));

    await waitFor(() => expect(screen.getByText(/Compilation succeeded/)).toBeInTheDocument());
    expect(globalThis.fetch).toHaveBeenCalledWith(
      compilerEndpoint,
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ language: "rust", source: "fn main() {}", network: "testnet" }),
      })
    );
    expect(screen.getByText("review the contract")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Deploy to Testnet" }));

    await waitFor(() => expect(freighter.signTransaction).toHaveBeenCalledWith(
      "AAAA",
      expect.objectContaining({ address: "GABC", networkPassphrase: TESTNET_PASSPHRASE })
    ));
    await waitFor(() => expect(screen.getByText("Transaction: tx-hash")).toBeInTheDocument());
    expect(screen.getByText(/Transaction pending on Testnet: tx-hash/)).toBeInTheDocument();
    expect(freighter.getNetwork).toHaveBeenCalled();
  });

  it("does not allow a custom network passphrase", () => {
    render(<SorobanIDE networkPassphrase="Public Global Stellar Network ; September 2015" />);
    expect(screen.getByRole("alert")).toHaveTextContent(/only supports the Stellar Testnet passphrase/i);
  });

  it("stops deployment when Freighter is not on Testnet", async () => {
    (globalThis.fetch as jest.Mock).mockResolvedValue(
      jsonResponse({ transactionXdr: "AAAA" })
    );
    freighter.getAddress.mockResolvedValue({ address: "GABC" });
    freighter.getNetwork.mockResolvedValue({
      network: "PUBLIC",
      networkPassphrase: "Public Global Stellar Network ; September 2015",
    });
    freighter.signTransaction.mockResolvedValue({ signedTxXdr: "SIGNED-XDR" });

    render(<SorobanIDE compilerEndpoint={compilerEndpoint} />);
    fireEvent.click(screen.getByRole("button", { name: "Compile Rust" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Deploy to Testnet" })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Deploy to Testnet" }));

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(/switched to Testnet/i));
    expect(freighter.signTransaction).not.toHaveBeenCalled();
  });
});
