# 发布 nipa-agent

`nipa-agent` 遵循语义化版本，使用名为 `vX.Y.Z` 的注释 Git 标签。在 `0.x`
阶段，破坏性公共 API 改动提升 minor 版本。

## 发布清单

1. 更新 `Cargo.toml` 的 `version`，并把对应内容从 Changelog 的“未发布”移动到
   带日期的版本节；
2. 运行完整发布门禁：

   ```bash
   cargo fmt -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets
   RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
   cargo package
   ```

3. 使用 `Release nipa-agent X.Y.Z` 提交；
4. 创建注释标签：`git tag -a vX.Y.Z -m "nipa-agent X.Y.Z"`；
5. 推送提交和标签，根据 `CHANGELOG.md` 创建 GitHub Release；
6. 更新 NipaServer 的 submodule 指针和 Cargo 精确版本约束并跑完整 CI。

创建 GitHub Release 不代表自动发布 crates.io。向 crates.io 发布是独立操作，必须
单独确认包名、所有权和发布凭证。
