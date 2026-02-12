# typopotamus

A Rust monorepo for discovering and downloading web fonts from any website.

## Academic Use Notice

This project is intended for academic and research purposes only.
Use it responsibly and only in ways that comply with website terms, applicable licenses, and relevant laws.

## Workspace Crates

- `typopotamus-core`: shared extraction, grouping, selection, and download logic.
- `typopotamus-tui`: interactive terminal UI built with `ratatui`.
- `typopotamus-cli`: non-interactive CLI built with `clap`.

## Build and Lint

```bash
cd typopotamus
cargo fmt
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## TUI

```bash
cargo run -p typopotamus-tui -- --url https://example.com
```

TUI key shortcuts:

- `Tab`: switch between families and font variants
- `Space`: toggle current selection
- `f`: toggle selection for current family
- `a`: toggle selection for all fonts
- `d`: download selected fonts

## CLI

Inspect fonts:

```bash
cargo run -p typopotamus-cli -- inspect --url https://www.apple.com
```

Inspect individual font files instead of grouped families:

```bash
cargo run -p typopotamus-cli -- inspect --url https://www.apple.com --view font
```

Inspect fonts as JSON (for agents/scripts):

```bash
cargo run -p typopotamus-cli -- inspect --url https://www.apple.com --format json
```

Download all fonts:

```bash
cargo run -p typopotamus-cli -- download --url https://www.apple.com --all
```

Download only one family:

```bash
cargo run -p typopotamus-cli -- download --url https://www.apple.com --family "SF Pro Text"
```

Download specific variants by inspect index:

```bash
cargo run -p typopotamus-cli -- download --url https://www.apple.com --index 49 --index 58
```
