# Contributing

`nipa-agent` is intentionally a small runtime with no media-domain, database,
web-framework or UI dependencies. Changes should preserve that boundary and
the public contract consumed by NipaServer.

Before submitting a change, run:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

Breaking public API or event-schema changes require a minor version bump while
the crate is in `0.x`, an updated changelog, and a synchronized update to
NipaServer's `docs/03-agent接口契约.md` and submodule pointer.

Do not commit provider API keys, live request captures, private prompts or
model responses containing user media data. Integration tests should use
wiremock and remain deterministic without network access.
