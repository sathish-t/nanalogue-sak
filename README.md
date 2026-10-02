# nanalogue-sak

A Swiss army knife of Rust tools built on [nanalogue](https://github.com/DNAReplicationLab/nanalogue).
Each tool lives in its own Cargo package within this workspace, not a nested Git repository.

## Packages

- [`nanalogue_adaptive_sampling_monitor`](adaptive_sampling_monitor/README.md) — live per-region read counts and lengths from MinKNOW BAM output

## Building and checking

Rust 1.99.0, Clippy and rustfmt are pinned in `rust-toolchain.toml`; rustup selects
and installs that toolchain automatically. Upgrade the pin deliberately together
with any resulting lint fixes. The native HTSlib dependencies need a C compiler,
CMake, pkg-config, OpenSSL, zlib, bzip2, xz/liblzma and libclang. For Debian 12, a
suitable setup is:

```sh
sudo apt-get install build-essential cmake pkg-config clang-19 libclang-19-dev \
  libssl-dev zlib1g-dev libbz2-dev liblzma-dev
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Nanalogue is pinned to a revision of its `main` branch. All upstream Cargo lint
settings are copied unchanged into `[workspace.lints]`; each member uses
`[lints] workspace = true`. New projects should inherit these settings too.
The lockfile is shared and tracked. Tests generate small, coordinate-sorted and
indexed BAM fixtures locally and need neither MinKNOW nor sequencing hardware.

## Agent skills

The `write-discoverable-code` skill under `.agents/skills/` was obtained from
[modem-dev/skills](https://github.com/modem-dev/skills). It is MIT licensed; see
[the included license](.agents/skills/write-discoverable-code/LICENSE).
