---
name: yunshu-cli
description: 操作云书无头节点 yunshu-cli：局域网传文件/发文本/飞鸽主机管理、pigeonbot e2e 联调、把它作为 MCP server 接入 AI 客户端。当用户提到云书 CLI、无头节点、局域网给某台电脑传文件发消息、跑云书 e2e，或要调试 yunshu MCP 工具时使用——即使用户没说"无头"两个字。
---

# yunshu-cli 无头节点

yunshu-cli 内嵌 yunshu-core（与桌面版同一套实现），一个二进制两种身份：直接运行是交互式终端；`--mcp` 是 MCP stdio server。仓库根目录构建：

```bash
cargo build -p yunshu-cli          # debug；发布用 --release
```

## 身份与参数（先读这段）

- `--home <目录>` 指定配置目录，**默认 `~/.yunshu-cli`，与桌面实例身份隔离**（独立机器 ID、端口、信任表）。测试永远用 `/tmp` 下的临时目录，别碰用户真实身份。
- `--port`：HTTP/WS 端口。默认 34256（与桌面实例冲突）；两个 CLI 实例并存时各自传不同端口。
- `--no-dnd`：关防打扰。**首次信任一台新设备前必须关**——防打扰开启时未固定指纹的设备会在握手阶段被静默拒绝，连信任确认都不会弹。
- `--name`、`--stealth`：广播名、隐身模式。
- 所有覆盖只影响本次运行，不写入 config.toml。

## 交互模式：两种驱动方式

### 一次性查询（管道队列）

命令在 stdin 里排队，逐条执行，`quit` 收尾：

```bash
printf 'status\npeers\nquit\n' | ./target/debug/yunshu-cli --home /tmp/ys-t1 --port 34278
```

适合 `status` / `peers` / `log` 这类立即返回的命令。**不适合**需要等异步事件的操作（发现、传输），因为管道没有等待能力。

### 需要等事件：用 drive.py

`scripts/drive.py` 按步骤驱动会话，支持 send / wait(正则) / sleep：

```bash
python3 .agents/skills/yunshu-cli/scripts/drive.py \
  --bin target/debug/yunshu-cli --home /tmp/ys-t1 --no-dnd -- \
  sleep:3 send:peers "wait:已连接:60" "send:sendfile <peer_id> /path" "wait:完成:600" send:quit
```

常用 wait 锚点：`本机 id`（启动完成）、`等待信任确认`、`已连接 1`（链路建立）、`发来批次`、`传输\] .*：完成`（批次终态）、`✓ 已发送`。

## 命令与安全门

`peers` / `status` / `log [n]` 只读；`send <peer> <文本>`、`sendfile <peer> <路径…>`（空格分隔多个路径，路径不能含空格）执行动作；`trust` / `rejecttrust` / `accept` / `reject` 是**人工门**。

**agent 必须遵守**：看到 `⚠ 新设备「X」等待信任确认（指纹 …）` 或 `⚠ 「X」发来批次…` 时，把提示原文转述给用户等决策，**不要自动执行 trust/accept**。指纹（TOFU）是身份安全的根，交给模型自动确认等于没有这道门。

## MCP 模式

```bash
./target/release/yunshu-cli --mcp --home /tmp/ys-mcp
```

七个工具。飞鸽（PC↔PC，需对方确认）：`pigeon_list_peers`、`pigeon_send_text`、`pigeon_send_files`（`wait_secs` 控制等终态的时长，默认 300，0 只发起不等）。微信通道（个人微信机器人，发给扫码绑定的机主）：`weixin_status`、`weixin_send_text`、`weixin_send_files`、`weixin_inbox`。信任/来件确认刻意不是工具——同上，人工门。

**微信通道首次登录必须在交互终端**：`wx login` 出二维码扫码（MCP 模式扫不了），登录态持久在配置目录 `weixin.json`，之后 `--mcp` 启动自动恢复长轮询；机主给机器人发的消息用 `weixin_inbox` 读取（媒体已自动解密落盘）。

接入 ZCode：`~/.zcode/cli/config.json` 加（workspace 共享则放 `<repo>/.zcode/config.json`）：

```json
{
  "mcp": {
    "servers": {
      "yunshu": {
        "command": "/绝对路径/yunshu",
        "args": ["--mcp", "--home", "/tmp/ys-mcp"]
      }
    }
  }
}
```

重启会话后工具以 `mcp__yunshu__*` 出现，直接调即可。

## e2e 联调（配 pigeonbot）

```bash
cargo build --release -p pigeonbot
./target/release/pigeonbot 1000000 150    # bot 推 1MB 批次 + 等来件 150 秒
```

流程与坑（都实测过）：

1. **主动连接权掷硬币**：约定 id 小的一方主动外连，而 app 侧 peer id 是每次启动的随机 UUID，bot 又只被动 accept。没连上就重启 CLI 再试（bot 端 id 固定，CLI 端重掷）。信任确认提示里的 `trust <id>` 在**信任建立后**才有效。
2. **首次信任流程**：CLI 主动外连未固定指纹的 bot → CLI 弹信任确认、挂起连接 → bot 进程退出 → 在 CLI 里 `trust <id>` → 重启 bot → 自动重连成功。之后指纹固定，`--no-dnd` 都可以省。
3. **bot 产物在 `$TMPDIR`**：`$TMPDIR/pigeonbot_recv/<批次id>/out/`，macOS 上不是 `/tmp`。
4. **配置目录迁移陷阱**：`ensure_config_dir` 在新目录缺 `pigeon_key.bin` 时会把旧系统配置目录（`~/Library/Application Support/yunshu`）的文件整个搬进来——覆盖你预置的 config.toml。要隔离就先预置一个 32 字节的 `pigeon_key.bin`。
5. **pigeonbot 不支持手动添加主机**（addhost 报"对方版本过旧"），别走 addhost，靠发现广播。
6. bot 推来的批次在 CLI 里是 `accept <offer_id前缀>`；也可以给 CLI 进程设 `YUNSHU_AUTO_ACCEPT=1` 自动接受（仅测试环境）。

## 排查速查

| 症状 | 原因与处置 |
|---|---|
| 列表有节点但互等不连 | id 掷硬币输了，重启 CLI 重掷 |
| 握手后无响应、无信任提示 | 防打扰开着，加 `--no-dnd` |
| save_dir 不生效 | 配置目录被迁移覆盖，预置 pigeon_key.bin 后重来 |
| 端口占用 / 服务启动失败 | 换 `--port`；UDP 34255 支持同机多实例（REUSEPORT） |
| MCP 客户端解析失败 | stdout 被日志污染——CLI 已内置 stderr 日志，确认没改回去 |
