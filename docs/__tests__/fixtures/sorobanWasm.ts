const encoder = new TextEncoder();

function uleb128(value: number): number[] {
  const bytes: number[] = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value !== 0) byte |= 0x80;
    bytes.push(byte);
  } while (value !== 0);
  return bytes;
}

function name(text: string): number[] {
  const bytes = Array.from(encoder.encode(text));
  return [...uleb128(bytes.length), ...bytes];
}

function section(id: number, payload: number[]): number[] {
  return [id, ...uleb128(payload.length), ...payload];
}

function customSection(sectionName: string, base64Xdr: string): number[] {
  return section(0, [...name(sectionName), ...Array.from(Buffer.from(base64Xdr, "base64"))]);
}

// XDR produced with @stellar/stellar-sdk (`ScSpecEntry`, `ScMetaEntry` and
// `ScEnvMetaEntry` `.toXdr()`), stored pre-encoded so the fixture has no
// runtime encoder dependency:
//   spec:    fn hello(to: Address) -> Vec<Symbol>, doc "Greets an address."
//   meta:    rsver = 1.98.0
//   envMeta: interface version protocol 25, pre-release 0
const SPEC_XDR = "AAAAAAAAABJHcmVldHMgYW4gYWRkcmVzcy4AAAAAAAVoZWxsbwAAAAAAAAEAAAAAAAAAAnRvAAAAAAATAAAAAQAAA+oAAAAR";
const META_XDR = "AAAAAAAAAAVyc3ZlcgAAAAAAAAYxLjk4LjAAAA==";
const ENV_META_XDR = "AAAAAAAAABkAAAAA";

/**
 * A minimal Soroban-shaped module: one host import (`v.g`), one exported
 * function `hello` that calls it, and the three Soroban custom sections.
 */
export function buildSorobanWasm({ withSpec = true } = {}): Uint8Array {
  const i64 = 0x7e;
  const bytes = [
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    ...section(1, [1, 0x60, 2, i64, i64, 1, i64]),
    ...section(2, [1, ...name("v"), ...name("g"), 0x00, 0]),
    ...section(3, [1, 0]),
    ...section(7, [1, ...name("hello"), 0x00, 1]),
    // (func (param i64 i64) (result i64) local.get 0 local.get 1 call 0)
    ...section(10, [1, ...uleb128(8), 0, 0x20, 0, 0x20, 1, 0x10, 0, 0x0b]),
    ...(withSpec ? customSection("contractspecv0", SPEC_XDR) : []),
    ...customSection("contractenvmetav0", ENV_META_XDR),
    ...customSection("contractmetav0", META_XDR),
  ];
  return new Uint8Array(bytes);
}
