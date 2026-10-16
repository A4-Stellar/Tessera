import { xdr } from "@stellar/stellar-sdk";
import { disassembleWasm, formatSpecType, inspectWasm, WasmInspectionError } from "@/lib/wasm-inspector";
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
});
