# 参与贡献

感谢参与 `nipa-agent`。本项目刻意保持为轻量 runtime，不包含媒体业务、数据库、
Web 框架或 UI 依赖。改动应维持这一边界，并保持 NipaServer 所依赖的公共契约清晰。

提交前请运行：

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

在 `0.x` 阶段，破坏性公共 API 或事件 schema 改动需要提升 minor 版本，同时更新
Changelog、NipaServer 的 `docs/03-agent接口契约.md`、Cargo 精确版本约束和
submodule 指针。向后兼容的修复使用 patch 版本。

请勿提交 provider API key、真实请求抓包、私人 prompt，或包含用户媒体信息的模型
响应。集成测试应使用 wiremock，保持确定性并且不依赖网络。

提交贡献即表示你有权提交相关代码，并同意其按本项目 MIT License 发布。
