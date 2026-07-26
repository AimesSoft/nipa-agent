# 变更记录

本文档记录项目的重要变化。项目采用
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 结构和
[语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

## [0.1.1] - 2026-07-27

### 变更

- 以中文作为 README、贡献指南、安全政策和发布指南的主要语言；
- 完善 `Conversation` 管家模式、版本策略和宿主职责的文档说明。

## [0.1.0] - 2026-07-27

### 新增

- 带 `submit_result` 终止语义和轮数上限的 OpenAI-compatible 工具调用循环；
- 供 NipaServer 管家使用的多轮对话循环；
- provider 兼容标志、错误分类、退避重试和 `Retry-After` 支持；
- 伪工具调用恢复与畸形参数自纠；
- token、轮数、取消和工具输出大小护栏；
- 可供 SSE 与 transcript 共用的结构化事件信封；
- 覆盖 provider 怪癖、重试、取消和事件不变量的 wiremock 测试；
- MIT 许可证、独立 CI、发布流程与项目维护文档。

[未发布]: https://github.com/AimesSoft/nipa-agent/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/AimesSoft/nipa-agent/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/AimesSoft/nipa-agent/releases/tag/v0.1.0
