# tessera-cli

`tessera-cli` generates deterministic, standalone Soroban Rust projects for tokenized real-world assets.

## Requirements

- Rust `1.81.0` or newer for the host CLI
- Rust `1.81.0` with `wasm32-unknown-unknown` for generated projects
- `soroban-sdk` `25.0.0`, matching the Tessera contract workspace

The generated project is self-contained and has no path dependency on this repository.

## Build

```bash
cargo build --manifest-path cli/Cargo.toml
```

Run the CLI from the repository root or build the binary and place it on `PATH`:

```bash
cargo run --manifest-path cli/Cargo.toml -- init my-rwa-asset
```

## Interactive generation

`init` asks for an asset type, compliance model, and dividend strategy. The prompt accepts the displayed number or the corresponding name:

```bash
tessera-cli init my-rwa-asset
```

Choices are:

- Asset type: Real Estate, Invoice, Commodity, or Debt
- Compliance model: KYC, Allowlist, Permissionless, or Hybrid KYC + allowlist
- Dividend strategy: Proportional, Fixed per holder, Tiered, or None

## Automation

Provide all three selection flags for noninteractive use:

```bash
tessera-cli init my-rwa-asset \
  --asset-type real-estate \
  --compliance-model kyc \
  --dividend-strategy proportional \
  --non-interactive \
  --output-dir .
```

`--type`, `--compliance`, and `--dividend` are accepted aliases. Use `--defaults` to select Real Estate, KYC, and Proportional without prompting. The output directory must already exist.

Project names are single ASCII path components beginning with a letter and containing only letters, digits, hyphens, or underscores. Existing files, directories, and symlinks are never overwritten. Generation uses exclusive file creation and cleans up only files created by the current run if writing fails.

## Generated project

The output contains:

- `Cargo.toml` with Soroban SDK `25.0.0` and the contract release profile
- `rust-toolchain.toml` with Rust `1.81.0` and the WebAssembly target
- `src/lib.rs` with the selected compliance and dividend behavior
- `tests/contract.rs` with prewritten Soroban tests
- `README.md` and `.gitignore`

Run the generated tests with:

```bash
cd my-rwa-asset
cargo test
```

The templates are embedded in the binary, so generation does not access the network or read files from the Tessera repository.

## Verification

```bash
cargo fmt --manifest-path cli/Cargo.toml -- --check
cargo test --manifest-path cli/Cargo.toml
cargo clippy --manifest-path cli/Cargo.toml --all-targets -- -D warnings
cargo build --manifest-path cli/Cargo.toml
```
