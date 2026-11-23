import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import LZString from "lz-string";
import {
  isSecureEndpoint,
  normalizeCompilerResponse,
  SorobanIDE,
  ASSET_TOKEN_SOURCE,
} from "../../components/SorobanIDE";
import {
  getCompletions,
  validateSorobanRust,
  createSorobanLspWorker,
} from "../../lib/soroban-lsp-worker";
import { compileSorobanRust } from "../../lib/soroban-inbrowser-compiler";
import { executeHostCall } from "../../lib/soroban-host-emulator";

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
    arrayBuffer: jest.fn().mockResolvedValue(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]).buffer),
  } as unknown as Response;
}

describe("SorobanIDE Full In-Browser IDE", () => {
  const originalFetch = globalThis.fetch;
  const freighter = {
    getAddress: jest.fn(),
    getNetwork: jest.fn(),
    signTransaction: jest.fn(),
  };

  beforeEach(() => {
    jest.clearAllMocks();
    window.localStorage.clear();
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
  });

  // (1) Monaco Editor with custom LSP server for Soroban Rust syntax highlighting and autocomplete
  it("provides custom LSP completions and diagnostics for Soroban Rust", () => {
    const completions = getCompletions("contract");
    expect(completions.some((c) => c.label === "#[contract]")).toBe(true);
    expect(completions.some((c) => c.label === "#[contractimpl]")).toBe(true);

    const envCompletions = getCompletions("storage");
    expect(envCompletions.some((c) => c.label.includes("env.storage"))).toBe(true);

    const diagnostics = validateSorobanRust("fn foo() {}");
    expect(diagnostics.some((d) => d.message.includes("#![no_std]"))).toBe(true);

    // Verifies LSP Web Worker creation helper
    const worker = createSorobanLspWorker();
    if (worker) {
      expect(typeof worker.postMessage).toBe("function");
      worker.terminate();
    }
  });

  // (2) compile-to-WASM using a wasm-pack pre-built binary served from /public/wasm/soroban-compiler.wasm
  it("compiles Rust source to WASM using the pre-built compiler binary", async () => {
    (globalThis.fetch as jest.Mock).mockResolvedValue(
      jsonResponse({ ok: true })
    );

    const result = await compileSorobanRust(ASSET_TOKEN_SOURCE, "/wasm/soroban-compiler.wasm");
    expect(result.success).toBe(true);
    expect(result.wasmBytes).toBeDefined();
    expect(result.wasmBytes![0]).toBe(0x00);
    expect(result.wasmBytes![1]).toBe(0x61); // 'a'
    expect(result.wasmBytes![2]).toBe(0x73); // 's'
    expect(result.wasmBytes![3]).toBe(0x6d); // 'm'
    expect(result.functions).toContain("transfer");
    expect(result.functions).toContain("balance");
  });

  // (3) sandboxed execution environment using a Soroban host emulator compiled to WASM in an iframe with sandbox="allow-scripts" CSP
  // (4) display resource usage (CPU instructions, memory, storage reads/writes) per invocation with asset-token transfer
  it("executes the asset-token contract transfer function inside sandboxed emulator and displays resource usage", async () => {
    render(<SorobanIDE />);

    // Check that the sandboxed iframe exists with sandbox="allow-scripts"
    const iframe = screen.getByTitle("Soroban Host Emulator Sandbox");
    expect(iframe).toBeInTheDocument();
    expect(iframe.getAttribute("sandbox")).toBe("allow-scripts");

    // Click "Invoke transfer()"
    const invokeBtn = screen.getByRole("button", { name: "Invoke transfer()" });
    fireEvent.click(invokeBtn);

    // Verify Invocation succeeded and resource usage metrics are displayed
    await waitFor(() => {
      expect(screen.getByText("Invocation Succeeded")).toBeInTheDocument();
    });

    const resourcePanel = screen.getByTestId("resource-usage-panel");
    expect(resourcePanel).toBeInTheDocument();
    expect(resourcePanel).toHaveTextContent("CPU Instructions");
    expect(resourcePanel).toHaveTextContent("Memory Usage");
    expect(resourcePanel).toHaveTextContent("Storage Reads");
    expect(resourcePanel).toHaveTextContent("Storage Writes");

    // Verify direct host emulator semantics for asset-token transfer
    const res = executeHostCall("transfer", {
      from: "GBBD...TESSERA",
      to: "GCXK...INVESTOR",
      amount: "25000",
    });
    expect(res.success).toBe(true);
    expect(res.metrics.cpuInstructions).toBeGreaterThan(0);
    expect(res.metrics.storageReads).toBe(3); // base + 2 balances
    expect(res.metrics.storageWrites).toBe(2); // sender + recipient balances
    expect(res.events[0].topic).toEqual(["transfer", "GBBD...TESSERA", "GCXK...INVESTOR"]);
  });

  // (5) persist editor state in localStorage with LZ-string compression
  it("persists editor state in localStorage with LZ-string compression", async () => {
    const { unmount } = render(<SorobanIDE />);
    const editor = screen.getByRole("textbox", { name: "Rust source editor" });

    const updatedCode = "#![no_std]\npub struct CustomContract;";
    fireEvent.change(editor, { target: { value: updatedCode } });

    const storedCompressed = window.localStorage.getItem("soroban_ide_code");
    expect(storedCompressed).toBeTruthy();
    const decompressed = LZString.decompressFromUTF16(storedCompressed!);
    expect(decompressed).toBe(updatedCode);

    unmount();

    // Re-rendering should restore from compressed localStorage
    render(<SorobanIDE />);
    expect(screen.getByRole("textbox", { name: "Rust source editor" })).toHaveValue(updatedCode);
  });

  // (6) add a Share button generating a compressed URL-safe snippet
  it("generates a compressed URL-safe snippet when Share button is clicked", async () => {
    Object.assign(navigator, {
      clipboard: {
        writeText: jest.fn().mockResolvedValue(undefined),
      },
    });

    render(<SorobanIDE />);
    const shareBtn = screen.getByRole("button", { name: "Share" });
    fireEvent.click(shareBtn);

    await waitFor(() => {
      expect(screen.getByText(/Snippet copied to clipboard|Snippet URL generated/)).toBeInTheDocument();
    });

    expect(navigator.clipboard.writeText).toHaveBeenCalled();
    const shareUrl = (navigator.clipboard.writeText as jest.Mock).mock.calls[0][0];
    expect(shareUrl).toContain("?snippet=");

    const snippet = shareUrl.split("?snippet=")[1];
    const decompressed = LZString.decompressFromEncodedURIComponent(snippet);
    expect(decompressed).toBe(ASSET_TOKEN_SOURCE);
  });

  it("does not allow a custom network passphrase", () => {
    render(<SorobanIDE networkPassphrase="Public Global Stellar Network ; September 2015" />);
    expect(screen.getByRole("alert")).toHaveTextContent(/only supports the Stellar Testnet passphrase/i);
  });
});
