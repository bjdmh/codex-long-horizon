## Installing & building

### System requirements

| Requirement                 | Details                                                         |
| --------------------------- | --------------------------------------------------------------- |
| Operating systems           | macOS 12+, Ubuntu 20.04+/Debian 10+, or Windows 11 **via WSL2** |
| Git (optional, recommended) | 2.23+ for built-in PR helpers                                   |
| RAM                         | 4-GB minimum (8-GB recommended)                                 |

### DotSlash

The GitHub Release also contains a [DotSlash](https://dotslash-cli.com/) file for the Codex CLI named `codex`. Using a DotSlash file makes it possible to make a lightweight commit to source control to ensure all contributors use the same version of an executable, regardless of what platform they use for development.

### Build from source

```bash
# Clone the repository and navigate to the root of the Cargo workspace.
git clone https://github.com/openai/codex.git
cd codex/codex-rs

# Install the Rust toolchain, if necessary.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustup component add rustfmt
rustup component add clippy
# Install helper tools used by the workspace justfile:
cargo install just
# Optional: install nextest for the `just test` helper
cargo install --locked cargo-nextest

# Build Codex.
cargo build

# Fastest local rebuild when you only need the main CLI binary.
cargo build -p codex-cli --bin codex

# The debug binary is usually the best choice for day-to-day development.
./target/debug/codex --help

# Launch the TUI with a sample prompt.
cargo run --bin codex -- "explain this codebase to me"

# Run exec mode without rebuilding a separate release binary.
./target/debug/codex exec "summarize the current repository"

# Build the optimized release binary only when you need a production-style
# executable for distribution or final verification.
cargo build --release -p codex-cli --bin codex
./target/release/codex --help

# After making changes, use the root justfile helpers (they default to codex-rs):
just fmt
just fix -p <crate-you-touched>

# Run the relevant tests (project-specific is fastest), for example:
cargo test -p codex-tui
# If you have cargo-nextest installed, `just test` runs the test suite via nextest:
just test
# Avoid `--all-features` for routine local runs because it increases build
# time and `target/` disk usage by compiling additional feature combinations.
# If you specifically want full feature coverage, use:
cargo test --all-features
```

### Fast local builds

For routine development, prefer the debug build of the main CLI binary:

```bash
cargo build -p codex-cli --bin codex
```

That command reuses incremental build artifacts and writes the executable to
`codex-rs/target/debug/codex`, which is usually much faster than a full
workspace build or a release build.

Use a release build only when you specifically need the optimized shipping
binary:

```bash
cargo build --release -p codex-cli --bin codex
```

This workspace configures release builds with fat LTO and a single codegen
unit in `codex-rs/Cargo.toml`, which keeps the final binary small and optimized
but makes linking much slower than debug builds.

## Tracing / verbose logging

Codex is written in Rust, so it honors the `RUST_LOG` environment variable to configure its logging behavior.

The TUI defaults to `RUST_LOG=codex_core=info,codex_tui=info,codex_rmcp_client=info` and log messages are written to `~/.codex/log/codex-tui.log` by default. For a single run, you can override the log directory with `-c log_dir=...` (for example, `-c log_dir=./.codex-log`).

```bash
tail -F ~/.codex/log/codex-tui.log
```

By comparison, the non-interactive mode (`codex exec`) defaults to `RUST_LOG=error`, but messages are printed inline, so there is no need to monitor a separate file.

See the Rust documentation on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for more information on the configuration options.
