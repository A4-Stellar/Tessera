# Token economics WebAssembly module

`cargo test` here is the **numerical stability test** for the agent-based
model: determinism for a seed, no NaN/Inf/negative prices under extreme
parameters, monotonic progress, and an arbitrage convergence check.

Build the browser module with:

```sh
./build.sh
# or
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/tessera_token_economics.wasm ../../public/wasm/token_economics.wasm
```

The module exposes a deliberately small numeric ABI (no `wasm-bindgen`):

| Export | Meaning |
|---|---|
| `tessera_alloc(len)` | allocate `len` bytes; caller writes 14 little-endian f64 params |
| `tessera_sim_new(ptr, len)` | create a simulation, returns a handle |
| `tessera_sim_step(handle)` | advance one step; returns `1` while incomplete |
| `tessera_sim_progress(handle)` | fraction complete in `[0, 1]` |
| `tessera_sim_write_output(handle)` | serialise results to JSON in linear memory |
| `tessera_sim_output_ptr()` / `tessera_sim_output_len()` | read that JSON |
| `tessera_sim_free(handle)` | release the simulation |

`docs/lib/token-economics-worker.ts` drives the WASM module step-by-step from a
Web Worker and posts `{ type: "progress", value }` messages to the UI. If the
binary has not been built (for example in a fresh checkout or jsdom), the
worker transparently uses the TypeScript reference engine in
`docs/lib/token-economics-engine.ts`, which implements the same model.
