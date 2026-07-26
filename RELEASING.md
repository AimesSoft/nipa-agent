# Releasing nipa-agent

`nipa-agent` uses Semantic Versioning and annotated Git tags named `vX.Y.Z`.
During `0.x`, breaking public API changes require a minor version bump.

## Release checklist

1. Update `version` in `Cargo.toml` and move changelog entries out of `Unreleased`.
2. Run the complete local gate:

   ```bash
   cargo fmt -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets
   RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
   cargo package
   ```

3. Commit with `Release nipa-agent X.Y.Z`.
4. Create an annotated tag: `git tag -a vX.Y.Z -m "nipa-agent X.Y.Z"`.
5. Push the commit and tag, then create GitHub release notes from `CHANGELOG.md`.
6. Update NipaServer's submodule pointer and exact Cargo version requirement.

Publishing to crates.io is a separate, explicit operation and is not implied
by creating a GitHub release.
