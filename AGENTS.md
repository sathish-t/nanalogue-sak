# Repository context

This Rust workspace includes the `adaptive_sampling_monitor` binary and may
contain additional workspace members over time. Nanalogue is pinned as a Git
dependency. The adaptive sampling monitor tests generate local indexed BAM
fixtures and require neither MinKNOW nor sequencing hardware.

# Verification

After changing code, run these commands in order:

```sh
cargo test -q --locked --workspace --all-targets
cargo clippy -q --locked --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo doc -q
```

Tests deliberately exercise missing, stale, and invalid BAM indexes. HTSlib may
emit warnings on stderr; use the Cargo test result to determine success.

# Final review

If you have access to a tool called the Oracle, ask it for its opinion on the
code changes before committing and address any findings you judge worth fixing.
Only perform this review when the Oracle tool is available; otherwise, skip it.

# Native dependencies

HTSlib requires Clang and native compression libraries. Orbs install Clang 19
and the required libraries through `.agents/setup`; in other environments,
follow the dependency instructions in `README.md`.

Use quiet Cargo commands wherever possible.
