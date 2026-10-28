# 云书 — 项目约定

## Git 提交信息

使用标准 Conventional Commits 风格：`<type>: <简述>`，中文简述。

长度控制在 **10-100 字**：一行说完，写清改了什么、为什么，不堆细节（细节留给代码与 PR 描述）。

常用 type：

- `feat:` 新功能
- `fix:` 修复缺陷
- `ci:` 构建 / 打包 / GitHub Actions
- `doc:` 文档
- 其他按需：`refactor:`、`perf:`、`test:`、`chore:`

示例：`feat: 头像状态灯按四色显示在线/隐身/免打扰/离线`
