# 安全政策

项目快速开发期间，只支持最新的 `0.x` 标签版本和当前 `main` 分支。

请使用 `AimesSoft/nipa-agent` 仓库的 **Security → Report a vulnerability**
私密报告功能。不要在公开 Issue 中提交可用攻击载荷、凭证或私人模型响应。

适合报告的问题包括：

- API key 或敏感 header 泄漏；
- 不安全的 URL、响应或历史消息处理；
- prompt、工具和宿主权限边界被绕过；
- 无界资源消耗或取消失效；
- 事件流和 transcript 泄漏。

上游模型服务自身的问题通常不在本项目范围内；但如果 runtime 对其响应处理不安全，
仍属于应报告范围。
