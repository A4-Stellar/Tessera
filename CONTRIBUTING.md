# Contributing to Tessera

Thanks for your interest in Tessera! Community contributions are welcome across **all sections of the repository**. Maintainer contact: Afolabi (`afolabiaderonke1995@gmail.com`), Repository: `https://github.com/A4-Stellar/Tessera`.


## Scope & Guidelines

This repository contains two main projects:

- **`docs/`** — the Next.js + MDX documentation site.
- **`api/`** — the Rust indexing service.

Contributions are welcome on **any section of the repository** (including `api/`, `docs/`, configuration, and tooling). All pull requests will be manually reviewed and decided on by the maintainer prior to merging.

## What makes a good contribution

- **Accuracy first.** Code examples and API logic must be accurate against the deployed contracts and platform specifications.
- **No placeholders.** No "TODO", no "coming soon", no lorem ipsum.
- **Match the codebase style.** Follow existing coding patterns, Rust idioms in `api/`, and design component patterns in `docs/`.
- **Pass builds and tests.** Ensure all local build, lint, and test suites pass before opening a PR.

## Local setup

This monorepo contains two independent projects with separate toolchains:

### Docs (Next.js + MDX)

```bash
cd docs
npm install
npm run dev      # http://localhost:3000
npm run build    # must pass before you open a PR
npm run lint     # Check for style issues
```

### API (Rust)

The API uses standard Rust tooling:

```bash
cd api
cargo build           # Compile the API
cargo test            # Run unit tests
cargo fmt --check     # Check code formatting (Rust style)
cargo clippy          # Lint for common mistakes and idioms
cargo fmt             # Auto-format code (apply changes)
```

Our CI runs these checks on every push (see `.github/workflows/`), so ensure these commands pass before submitting your PR.

## Submitting a PR

1. Fork and branch from `main`.
2. Make your changes in any section of the repository (`docs/`, `api/`, root configs, etc.).
3. Run relevant build, lint, and test commands (`npm run build` in `docs/`, `cargo test` in `api/`) to verify your changes.
4. Open a PR with a clear description of your changes and motivation.
5. The maintainer will manually review the pull request and decide whether to merge it.

## Reporting issues

Found a problem or have a feature proposal? Open an issue with:

- clear steps to reproduce or context for your proposal,
- the expected vs. actual behavior (if reporting a bug),
- any relevant logs or endpoints involved.

## Releasing (maintainers only)

The api crate is versioned from `api/Cargo.toml`, and the `/` endpoint surfaces that same version to consumers. The release process is documented in [RELEASING.md](./RELEASING.md).

