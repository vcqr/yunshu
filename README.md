# 云书 · 局域网传书

> 云中谁寄锦书来 —— 局域网内电脑与手机互传文件和文本，手机扫码即用、零安装。

云书是一个 Windows 桌面常驻小工具：启动后缩在系统托盘，点开显示二维码，手机相机扫码在浏览器中打开，输入 6 位配对码完成配对，之后即可双向传输文件和文本。全程走局域网，不经过任何服务器。

## 功能

- **托盘常驻**：启动即后台服务，点托盘图标打开主窗口；关窗隐藏不退出，退出走托盘菜单
- **扫码配对**：二维码内含 60 秒轮换的一次性 token；6 位配对码二次确认，60 秒超时、5 次错误保护
- **文件互传**
  - 电脑 → 手机：选择文件后，手机网页出现卡片，点击即由浏览器原生下载
  - 手机 → 电脑：网页多选上传，电脑弹窗选择保存目录（2 分钟未选则存默认目录）
- **文本互传**：电脑剪贴板一键发给手机；手机文本一键进电脑剪贴板
- **传输记录**：进度条、状态、完成后可一键打开所在文件夹
- **设置**：电脑名称 / 端口（默认 34256）/ 保存目录 / 开机自启，TOML 持久化于 `%APPDATA%\yunshu\config.toml`

## 构建与运行

需要 Rust 1.85+（edition 2024）。已配置 rsproxy.cn 镜像（`.cargo/config.toml`，项目级，网络畅通环境可删）。

```bash
cargo run --release
```

产物为单文件 `target/release/yunshu.exe`（约 5 MB，无任何运行时依赖）。

### 前端开发（热重载）

```bash
cd frontend
npm run tauri:dev   # vite 热重载直达桌面窗口（首次为 debug 编译，较慢）
npm run dev         # 仅浏览器预览（无 Tauri 能力，窗口按钮等自动隐藏）
```

注意 Tauri 配置在 `crates/yunshu-app/tauri.conf.json`（非标准 `src-tauri` 布局），`tauri:dev` 已处理好目录跳转；vite 固定 5173 端口（strictPort），被占用时宁可报错也不静默漂移导致白屏。

## 使用步骤

1. 启动云书，托盘出现信封图标
2. 点托盘图标打开窗口，左侧显示二维码和局域网地址
3. 手机（与电脑同一局域网）相机扫码 → 浏览器打开 → 输入电脑上显示的 6 位配对码
4. 互传文件 / 文本

> 注：微信扫码打开时无法下载文件（微信内置浏览器限制），页面会提示——点右上角「···」→「在浏览器打开」后再操作；用系统相机扫码（Safari / Chrome）则无此限制。

## 本机测试（e2e）

`crates/pigeonbot` 是一个无头飞鸽测试节点，可在同一台机器上与云书实例完成 PC↔PC 双向传输的端到端验证（Noise XX 握手、批次邀请、逐文件 SHA-256 校验、暂停/恢复、断点续传）。

```bash
cargo build --release -p pigeonbot   # 构建测试节点

# 终端 1：启动云书主程序
./target/release/yunshu              # Windows 下为 yunshu.exe

# 终端 2：启动测试节点
./target/release/pigeonbot 1048576 60
```

bot 参数：第 1 个为 bot→app 推送批次的测试大小（默认 1MB）；第 2 个为推送完成后保持在线等待 app→bot 来件的秒数（默认 30，拖拽测试建议给大些，传 `0` 表示只测 bot→app 后即退）。

**bot→app 方向**：bot 每 2 秒 UDP 广播，app 飞鸽列表出现"e2e测试节点"并自动连入；握手后 bot 发一条加密聊天 + 三文件批次（1MB / 参数大小 / 0 字节，覆盖空文件与暂停恢复）。在 app 里确认接收后由 app 主动拉取，全部校验通过后 bot 打印 `E2E-BOT-DONE <总字节> <批次id>`，文件落在保存目录。

**app→bot 方向**（测拖拽/右键发送）：在等待窗口内把文件拖进 app 选 bot 发送，bot 无条件自动接受、收块写盘、逐文件校验，完成后打印 `E2E-BOT-RECV-DONE`，文件在 `/tmp/pigeonbot_recv/<批次id>/out/`。

### 单机双开互传

不用 bot，直接开两个云书实例互相传（CLI 覆盖只影响本次运行，不写入 config.toml）：

```bash
# 终端 1（实例 A，默认端口 34256）
./target/release/yunshu

# 终端 2（实例 B，换 HTTP/WS 端口 + 换广播名）
./target/release/yunshu --port 8001 --name 电脑B
```

两个窗口的飞鸽列表都会出现对方（实例 id 每次启动随机生成，必然互相可见；UDP 发现端口支持同机多实例共存：macOS 用 SO_REUSEPORT、Linux/Windows 用 SO_REUSEADDR），选中互发文件/文本即可。给任一实例设 `YUNSHU_AUTO_ACCEPT=1` 可自动接受来件批次，免点确认弹窗。

已知差异（不影响 PC↔PC 互传测试）：HTTPS 34257 只有一个，实例 B 会绑定失败并降级为无 HTTPS（手机端安全上下文功能不可用）；两个实例共享配置目录，静态密钥与信任表相同，互连一般不弹信任确认。

排查：

- 列表能看到节点但连不上——双方约定 id 小者发起连接，bot 只被动接受；bot id 固定来自 `/tmp/pigeonbot_key.bin`，删掉该文件重启 bot 即可换 id
- 换过 bot key 后连不上——app 已 TOFU 固定旧指纹，重置 `known_peers.json`（配置目录下，Windows 为 `%APPDATA%\yunshu`）
- 开了防打扰模式会直接拒绝未信任设备，测试时先关掉

纯逻辑单元测试不依赖网络：`cargo test`。

## 防火墙提示

首次运行 Windows 会弹窗询问是否允许访问网络，**务必允许**（专用网络）。若错过弹窗，手动放行（启动时固定监听 3 个端口）：

```powershell
netsh advfirewall firewall add rule name="yunshu-http"  dir=in action=allow protocol=TCP localport=34256
netsh advfirewall firewall add rule name="yunshu-https" dir=in action=allow protocol=TCP localport=34257
netsh advfirewall firewall add rule name="yunshu-disc"  dir=in action=allow protocol=UDP localport=34255
```

- TCP 34256：HTTP（手机配对/传输页面 + PC↔PC 飞鸽 WebSocket）
- TCP 34257：HTTPS（自签证书，手机浏览器安全上下文）
- UDP 34255：飞鸽发现广播（不放行则互相发现不了）

若发现"手机打不开页面"，先查：① 防火墙规则；② 路由器是否开启了 AP 隔离/访客网络；③ 电脑是否连了 VPN 虚拟网卡。

## 已知限制（MVP）

- 纯 HTTP 局域网明文传输，配对码 + 一次性 token 防蹭连，但传输内容不加密（后续可加）
- 手机浏览器剪贴板 API 需要 HTTPS 安全上下文，因此手机→电脑发文本用页面内粘贴的降级方案；电脑→手机的文本用"展示 + 手动复制"
- 大文件暂不支持断点续传
- 手机端同一时间只允许一台设备连接

## 技术栈

Rust workspace：

- `crates/yunshu-core` — axum（HTTP + WebSocket）、配对状态机、传输管理、arboard 剪贴板；手机端页面以 `include_str!` 内嵌
- `crates/yunshu-desktop` — eframe/egui（glow 后端）、tray-icon 托盘、fast_qr 二维码、rfd 文件对话框、winreg 开机自启

线程模型：tokio runtime 跑在后台线程，eframe 占主线程，`std::sync::mpsc` 双向桥接 + `request_repaint` 唤醒。

## 后续计划

- PC ↔ PC 飞鸽传书模式（UDP 广播发现 + 聊天）
- 配对码派生密钥的端到端加密
- 断点续传（HTTP Range）
- 手机端 PWA 化
