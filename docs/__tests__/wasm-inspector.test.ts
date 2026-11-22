import { xdr } from "@stellar/stellar-sdk";
import {
  analyzeWasmInstructionBudget,
  decodeWasmAst,
  disassembleWasm,
  executeWasmWithMockHost,
  extractContractMetadata,
  formatSpecType,
  inspectWasm,
  parseSorobanBudgetSection,
  WasmInspectionError,
} from "@/lib/wasm-inspector";
import { runWasmInspectorInWorker } from "@/lib/wasm-worker";
import { resolveHostFunction } from "@/lib/soroban-host-functions";
import { buildSorobanWasm } from "./fixtures/sorobanWasm";

describe("wasm inspector", () => {
  it("decodes exports, host imports and the Soroban custom sections", () => {
    const inspection = inspectWasm(buildSorobanWasm());

    expect(inspection.exports).toEqual([{ name: "hello", kind: "function", index: 1, signature: "(i64, i64) -> i64" }]);
    expect(inspection.imports).toEqual([
      {
        module: "v",
        field: "g",
        kind: "function",
        signature: "(i64, i64) -> i64",
        hostFunction: {
          module: "vec",
          name: "vec_new_from_linear_memory",
          signature: "(vals_pos: U32Val, len: U32Val) -> VecObject",
        },
      },
    ]);
    expect(inspection.spec).toEqual([
      { kind: "function", name: "hello", doc: "Greets an address.", details: ["fn hello(to: Address) -> Vec<Symbol>"] },
    ]);
    expect(inspection.meta).toEqual([{ key: "rsver", value: "1.98.0" }]);
    expect(inspection.interfaceVersion).toEqual({ protocol: 25, preRelease: 0 });
    expect(inspection.customSections).toEqual(["contractspecv0", "contractenvmetav0", "contractmetav0"]);
    expect(inspection.warnings).toEqual([]);
  });

  it("warns instead of failing when the contract spec section is missing", () => {
    const inspection = inspectWasm(buildSorobanWasm({ withSpec: false }));

    expect(inspection.spec).toEqual([]);
    expect(inspection.warnings).toEqual([
      "No `contractspecv0` section: this module does not look like a Soroban contract.",
    ]);
  });

  it("rejects input without a WebAssembly header", () => {
    expect(() => inspectWasm(new TextEncoder().encode("not wasm"))).toThrow(WasmInspectionError);
  });

  it("disassembles the module to WebAssembly text", () => {
    const wat = disassembleWasm(buildSorobanWasm()).join("\n");

    expect(wat).toContain('(import "v" "g")');
    expect(wat).toContain('(export "hello"');
    expect(wat).toContain("call $import0");
  });

  it("renders nested spec types with soroban-sdk names", () => {
    const { ScSpecTypeDef } = xdr;
    const type = ScSpecTypeDef.scSpecTypeResult(
      new xdr.ScSpecTypeResult({
        okType: ScSpecTypeDef.scSpecTypeOption(
          new xdr.ScSpecTypeOption({
            valueType: ScSpecTypeDef.scSpecTypeMap(
              new xdr.ScSpecTypeMap({
                keyType: ScSpecTypeDef.scSpecTypeAddress(),
                valueType: ScSpecTypeDef.scSpecTypeTuple(
                  new xdr.ScSpecTypeTuple({
                    valueTypes: [ScSpecTypeDef.scSpecTypeI128(), ScSpecTypeDef.scSpecTypeBytesN(new xdr.ScSpecTypeBytesN({ n: 32 }))],
                  })
                ),
              })
            ),
          })
        ),
        errorType: ScSpecTypeDef.scSpecTypeUdt(new xdr.ScSpecTypeUdt({ name: "Error" })),
      })
    );

    expect(formatSpecType(type)).toBe("Result<Option<Map<Address, (i128, BytesN<32>)>>, Error>");
  });

  it("only resolves genuine host functions", () => {
    expect(resolveHostFunction("x", "_")?.name).toBe("log_from_linear_memory");
    expect(resolveHostFunction("__proto__", "toString")).toBeNull();
    expect(resolveHostFunction("x", "toString")).toBeNull();
  });

  it("decodes a structured WebAssembly module AST with custom sections and instructions", () => {
    const ast = decodeWasmAst(buildSorobanWasm());
    expect(ast.type).toBe("Program");
    expect(ast.body[0].type).toBe("Module");
    expect(ast.body[0].customSections.map((s) => s.name)).toEqual([
      "contractspecv0",
      "contractenvmetav0",
      "contractmetav0",
    ]);
    expect(ast.body[0].instructions.some((instr) => instr.id === "call")).toBe(true);
  });

  it("parses the soroban_budget custom section and falls back to static instruction profiling", () => {
    expect(parseSorobanBudgetSection(buildSorobanWasm())).toBeNull();

    const wasmWithBudget = buildSorobanWasm({
      budget: {
        compute: 1_200_000,
        memory: 450_000,
        storage: 800_000,
        host_calls: 350_000,
        crypto: 200_000,
        mem_bytes: 2_097_152,
      },
    });
    const parsed = parseSorobanBudgetSection(wasmWithBudget);
    expect(parsed).not.toBeNull();
    expect(parsed?.source).toBe("soroban_budget");
    expect(parsed?.totalInstructions).toBe(3_000_000);
    expect(parsed?.memBytes).toBe(2_097_152);
    expect(parsed?.categories.find((c) => c.category === "compute")?.instructions).toBe(1_200_000);

    const staticProfile = analyzeWasmInstructionBudget(buildSorobanWasm());
    expect(staticProfile.source).toBe("static_analysis");
    expect(staticProfile.totalInstructions).toBeGreaterThan(0);
  });

  it("extracts contractenvmeta and contractmetav0 metadata including SDK version, contract name, and interface hash", () => {
    const wasm = buildSorobanWasm({
      envMetaJson: {
        contract_name: "tessera_rwa_token",
        sdk_version: "25.3.2#a1b2c3",
        protocol: 25,
      },
    });
    const metadata = extractContractMetadata(wasm, "token.wasm");
    expect(metadata.contractName).toBe("tessera_rwa_token");
    expect(metadata.sdkVersion).toBe("25.3.2");
    expect(metadata.rustVersion).toBe("1.98.0");
    expect(metadata.protocolVersion).toBe(25);
    expect(metadata.interfaceHash).toMatch(/^0x[0-9a-f]{16}$/);
  });

  it("executes a compiled .wasm module with mock Soroban host functions", async () => {
    const trace = await executeWasmWithMockHost(buildSorobanWasm(), "hello", [10n, 32n]);
    expect(trace.functionName).toBe("hello");
    expect(trace.returnValue).toBe("42");
    expect(trace.hostCalls).toEqual([
      {
        module: "v",
        field: "g",
        canonicalName: "vec::vec_new_from_linear_memory",
        args: ["10", "32"],
        returnValue: "42",
      },
    ]);
  });

  it("offloads WASM inspection through runWasmInspectorInWorker", async () => {
    const result = await runWasmInspectorInWorker(buildSorobanWasm(), "hello.wasm");
    expect(result.inspection.exports[0].name).toBe("hello");
    expect(result.metadata.contractName).toBe("hello");
    expect(result.lines.join("\n")).toContain("call $import0");
  });
});
