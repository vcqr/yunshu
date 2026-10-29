# yunshu-cli — 云书无头节点

云书的命令行形态：内嵌与桌面版同一套核心（yunshu-core），一个二进制两种身份——直接运行是交互式终端，`--mcp` 是 MCP stdio server（供 Claude Desktop / ZCode 等 AI 客户端调用）。

## 交互式终端

```bash
./yunshu-cli --home ~/.yunshu-cli
```

常用命令：`peers` / `status` / `send <peer> <文本>` / `sendfile <peer> <路径…>` / `trust <peer>` / `accept <批次>` / `quit`（`help` 查看全部）。

## MCP server（AI agent 接入）

```bash
./yunshu-cli --mcp --home ~/.yunshu-cli
```

工具：`pigeon_list_peers` / `pigeon_send_text` / `pigeon_send_files`。客户端配置示例（Claude Desktop 的 `claude_desktop_config.json` 或 ZCode 的 `~/.zcode/cli/config.json`）：

```json
{
  "mcpServers": {
    "yunshu": {
      "command": "/解压路径/yunshu-cli",
      "args": ["--mcp", "--home", "~/.yunshu-cli"]
    }
  }
}
```

Windows 下二进制名为 `yunshu-cli.exe`，`command` 指到它即可。

## 参数

- `--home <目录>`：配置目录，默认 `~/.yunshu-cli`（与桌面实例身份隔离：独立机器 ID、端口与信任表）；指到 `~/.yunshu` 可与桌面版共用身份
- `--port <n>`：HTTP/WS 端口（默认 34256，与桌面实例同开时换一个）
- `--name <名>`：局域网广播名
- `--no-dnd`：关防打扰（首次信任新设备前需要）
- `--stealth`：隐身模式（不广播本机）

## 安全默认

信任新设备、接受来件批次是人工门：CLI 终端里由人决策，MCP 模式刻意不暴露这两个操作——agent 无法替你信任一台设备或接收文件。

完整文档（e2e 联调、排查速查）见仓库 README「无头节点」一节。
