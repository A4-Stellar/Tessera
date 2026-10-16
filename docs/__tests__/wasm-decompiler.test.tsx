import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { xdr } from "@stellar/stellar-sdk";
import { WasmDecompiler } from "../components/WasmDecompiler";
import {
  parseWasm,
  toWat,
  extractContractSpecSection,
  WasmParseError,
} from "../lib/wasm";

function encodeULEB(value: number): number[] {
  const out: number[] = [];
  let remaining = value >>> 0;
  do {
    let byte = remaining & 0x7f;
    remaining >>>= 7;
    if (remaining !== 0) byte |= 0x80;
    out.push(byte);
  } while (remaining !== 0);
  return out;
}

function encodeName(text: string): number[] {
  const bytes = Array.from(new TextEncoder().encode(text));
  return [...encodeULEB(bytes.length), ...bytes];
}

function section(id: number, payload: number[]): number[] {
  return [id, ...encodeULEB(payload.length), ...payload];
}

function buildSimpleWasm(): Uint8Array {
  const magic = [0x00, 0x61, 0x73, 0x6d];
  const version = [0x01, 0x00, 0x00, 0x00];
  const typeSection = section(1, [1, 0x60, 0, 1, 0x7f]);
  const funcSection = section(3, [1, 0]);
  const exportSection = section(7, [1, ...encodeName("get_answer"), 0x00, 0]);
  const codeBody = [0x00, 0x41, 42, 0x0b];
  const codeSection = section(10, [1, ...encodeULEB(codeBody.length), ...codeBody]);
  const dataBytes = Array.from(new TextEncoder().encode("DataKey::Holders"));
  const dataSegment = [0x00, 0x41, 0x10, 0x0b, ...encodeULEB(dataBytes.length), ...dataBytes];
  const dataSection = section(11, [1, ...dataSegment]);
  return new Uint8Array([
    ...magic,
    ...version,
    ...typeSection,
    ...funcSection,
    ...exportSection,
    ...codeSection,
    ...dataSection,
  ]);
}

function buildSpecSectionBytes(): Uint8Array {
  const utf8 = (text: string): Uint8Array =>
    new Uint8Array(Buffer.from(text, "utf8"));
  const specEntry = new xdr.ScSpecFunctionV0({
    doc: utf8("Mints shares to an account"),
    name: utf8("mint"),
    inputs: [
      new xdr.ScSpecFunctionInputV0({
        doc: utf8("Recipient"),
        name: utf8("to"),
        type: xdr.ScSpecTypeDef.scSpecTypeAddress(),
      }),
      new xdr.ScSpecFunctionInputV0({
        doc: utf8(""),
        name: utf8("amount"),
        type: xdr.ScSpecTypeDef.scSpecTypeI128(),
      }),
    ],
    outputs: [xdr.ScSpecTypeDef.scSpecTypeBool()],
  });
  return xdr.ScSpecEntry.scSpecEntryFunctionV0(specEntry).toXDR();
}

function buildWasmWithSpec(): Uint8Array {
  const base = buildSimpleWasm();
  const specPayload = [
    ...encodeName("contractspecv0"),
    ...Array.from(buildSpecSectionBytes()),
  ];
  const custom = section(0, specPayload);
  const merged = new Uint8Array(base.length + custom.length);
  merged.set(base, 0);
  merged.set(custom, base.length);
  return merged;
}

function fileFrom(bytes: Uint8Array, name = "contract.wasm"): File {
  return new File([bytes as unknown as BlobPart], name, { type: "application/wasm" });
}

describe("wasm lib (pure parser)", () => {
  it("parses sections, exports and function bodies", () => {
    const parsed = parseWasm(buildSimpleWasm());
    expect(parsed.version).toBe(1);
    expect(parsed.typeSignatures).toHaveLength(1);
    expect(parsed.typeSignatures[0]).toContain("(result i32)");
    expect(parsed.exports).toEqual([{ name: "get_answer", kind: "func", index: 0 }]);
    expect(parsed.functions).toHaveLength(1);
    expect(parsed.functions[0].exportName).toBe("get_answer");
    expect(parsed.functions[0].code).toEqual(["  i32.const 42", "end"]);
    expect(parsed.dataStrings.map((entry) => entry.value)).toContain("DataKey::Holders");
    expect(parsed.errors).toEqual([]);
  });

  it("renders WAT text for the module", () => {
    const wat = toWat(parseWasm(buildSimpleWasm()));
    expect(wat.startsWith("(module")).toBe(true);
    expect(wat).toContain('(export "get_answer" (func 0))');
    expect(wat).toContain("i32.const 42");
    expect(wat).toContain('"DataKey::Holders"');
    expect(wat.trimEnd().endsWith(")")).toBe(true);
  });

  it("extracts the contractspecv0 custom section payload", () => {
    const specBytes = buildSpecSectionBytes();
    const parsed = parseWasm(buildWasmWithSpec());
    const sectionBytes = extractContractSpecSection(parsed);
    expect(sectionBytes).not.toBeNull();
    expect(Array.from(sectionBytes!)).toEqual(Array.from(specBytes));
    expect(extractContractSpecSection(parseWasm(buildSimpleWasm()))).toBeNull();
  });

  it("rejects files that are not WebAssembly", () => {
    expect(() => parseWasm(new TextEncoder().encode("definitely not wasm at all"))).toThrow(
      WasmParseError
    );
    expect(() => parseWasm(new TextEncoder().encode("definitely not wasm at all"))).toThrow(
      /magic header/i
    );
  });

  it("rejects truncated section payloads", () => {
    const valid = buildSimpleWasm();
    const truncated = valid.slice(0, 40);
    expect(() => parseWasm(truncated)).toThrow(WasmParseError);
  });
});

describe("WasmDecompiler component", () => {
  const originalCreateObjectUrl = URL.createObjectURL;

  beforeAll(() => {
    Element.prototype.scrollIntoView = jest.fn();
    URL.createObjectURL = jest.fn(() => "blob:test");
    Object.assign(navigator, {
      clipboard: { writeText: jest.fn().mockResolvedValue(undefined) },
    });
  });

  afterAll(() => {
    URL.createObjectURL = originalCreateObjectUrl;
  });

  beforeEach(() => {
    (navigator.clipboard.writeText as jest.Mock).mockClear();
    window.history.replaceState(null, "", window.location.pathname);
  });

  it("shows the empty state until a file is loaded", () => {
    render(<WasmDecompiler />);
    expect(screen.getByTestId("wasm-decompiler")).toBeInTheDocument();
    expect(screen.getByText(/Drop or open a compiled contract/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open .wasm file" })).toBeInTheDocument();
  });

  it("shows a parse error for invalid files", async () => {
    render(<WasmDecompiler />);
    const input = screen.getByLabelText("WebAssembly contract file");
    fireEvent.change(input, {
      target: { files: [fileFrom(new TextEncoder().encode("nope nope nope"), "bad.wasm")] },
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(/magic header/i);
  });

  it("disassembles a valid module into line-numbered WAT", async () => {
    render(<WasmDecompiler />);
    const input = screen.getByLabelText("WebAssembly contract file");
    fireEvent.change(input, { target: { files: [fileFrom(buildSimpleWasm())] } });

    expect(await screen.findByText(/i32\.const 42/)).toBeInTheDocument();
    expect(document.getElementById("wat-line-1")).toBeInTheDocument();
    expect(screen.getByText(/\(module/)).toBeInTheDocument();
  });

  it("supports interactive search with match counts and next navigation", async () => {
    render(<WasmDecompiler />);
    const input = screen.getByLabelText("WebAssembly contract file");
    fireEvent.change(input, { target: { files: [fileFrom(buildSimpleWasm())] } });
    await screen.findByText(/i32\.const 42/);

    const search = screen.getByLabelText("Search disassembly");
    fireEvent.change(search, { target: { value: "get_answer" } });
    expect(screen.getByTestId("wat-search-count")).toHaveTextContent("2 matches");

    fireEvent.change(search, { target: { value: "i32.const" } });
    expect(screen.getByTestId("wat-search-count")).toHaveTextContent("1 match");

    fireEvent.click(screen.getByRole("button", { name: "Next match" }));
    await waitFor(() => {
      expect(window.location.hash).toMatch(/^#wat-line-\d+$/);
    });
  });

  it("copies a deep link when a line number is clicked", async () => {
    render(<WasmDecompiler />);
    const input = screen.getByLabelText("WebAssembly contract file");
    fireEvent.change(input, { target: { files: [fileFrom(buildSimpleWasm())] } });
    await screen.findByText(/i32\.const 42/);

    fireEvent.click(screen.getByRole("button", { name: "Line 1" }));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(expect.stringContaining("#wat-line-1"));
    expect(window.location.hash).toBe("#wat-line-1");
  });

  it("lists exports and storage-key candidates", async () => {
    render(<WasmDecompiler />);
    const input = screen.getByLabelText("WebAssembly contract file");
    fireEvent.change(input, { target: { files: [fileFrom(buildSimpleWasm())] } });
    await screen.findByText(/i32\.const 42/);

    fireEvent.click(screen.getByRole("tab", { name: "Exports & Storage" }));
    expect(screen.getByText("get_answer")).toBeInTheDocument();
    expect(screen.getByText("DataKey::Holders")).toBeInTheDocument();
    expect(screen.getByText(/Exported functions \(1\)/)).toBeInTheDocument();
  });

  it("reports a missing Soroban spec section on the Interface tab", async () => {
    render(<WasmDecompiler />);
    const input = screen.getByLabelText("WebAssembly contract file");
    fireEvent.change(input, { target: { files: [fileFrom(buildSimpleWasm())] } });
    await screen.findByText(/i32\.const 42/);

    fireEvent.click(screen.getByRole("tab", { name: "Interface" }));
    expect(await screen.findByText(/No contractspecv0 section found/i)).toBeInTheDocument();
  });

  it("extracts and renders Soroban spec entries", async () => {
    render(<WasmDecompiler />);
    const input = screen.getByLabelText("WebAssembly contract file");
    fireEvent.change(input, { target: { files: [fileFrom(buildWasmWithSpec())] } });
    await screen.findByText(/i32\.const 42/);

    fireEvent.click(screen.getByRole("tab", { name: "Interface" }));
    expect(await screen.findByText(/fn mint\(to: address, amount: i128\) -> bool/)).toBeInTheDocument();
    expect(screen.getByText(/Mints shares to an account/)).toBeInTheDocument();
    expect(screen.getByText("1 spec entries")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy JSON schema" })).toBeInTheDocument();
  });

  it("lists Soroban custom sections on the storage tab", async () => {
    render(<WasmDecompiler />);
    const input = screen.getByLabelText("WebAssembly contract file");
    fireEvent.change(input, { target: { files: [fileFrom(buildWasmWithSpec())] } });
    await screen.findByText(/i32\.const 42/);

    fireEvent.click(screen.getByRole("tab", { name: "Exports & Storage" }));
    expect(screen.getByText(/contractspecv0 — \d+ bytes/)).toBeInTheDocument();
  });
});
