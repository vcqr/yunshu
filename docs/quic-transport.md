# 远程控制传输层升级：QUIC 设计方案

现状：远程控制走单条 TCP WebSocket（+Noise 加密）。TCP 队头阻塞意味着 Wi-Fi 丢一个包，
后面所有视频帧都要等重传；键鼠输入与视频同流，输入也会被视频阻塞。最新帧信箱解决了
"积压旧帧"，但解决不了"传输中的队头等待"。

目标：把传输层换成 QUIC（quinn crate），获得——

1. **输入零阻塞**：键鼠走独立可靠流（Stream 0），永远不被视频数据阻塞（现 TCP 下
   输入最多等一个帧发完）。
2. **视频抗丢包**：视频 delta 帧走**不可靠数据报**（datagram），丢一帧只丢一帧，
   不阻塞后续帧；关键帧与编码器参数走可靠流，保证流连续性。
3. **更快的弱网恢复**：QUIC 单拥塞窗口跨流共享 + 每包独立 ACK，配合已有
   "发送耗时自适应"，丢包后恢复以帧为单位而非以 RTT 为单位。
4. **0-RTT 重连**：会话重连可复用 QUIC 级 address validation（配合应用层自动重连）。

## 线路设计

端口：TCP 方案保留（兼容旧版），QUIC 监听 `cfg.port + 1`（UDP）。发现广播的
Announce 增加 `quic_port` 字段（serde default，旧版无此字段不协商 QUIC）。

握手与鉴权：沿用 Noise XX + RemoteAuth 流程不变（在 QUIC 可靠流上跑，复用现有
`pigeon::new_handshake` / `seal_msg` / `open_msg` 全部逻辑）——双加密有少量开销，
但协议层零改动、两套传输行为完全一致，风险最小。后续若要省掉双加密，再做
Noise-over-QUIC 的 PSK 绑定（把 QUIC TLS 指纹写进 Noise payload 防降级）。

消息通道：

| 通道 | QUIC 类型 | 内容 |
|------|-----------|------|
| control | 双向可靠流 0 | RemoteWire JSON（'C' 帧，现格式不变） |
| video | 单向可靠流 1 | 仅关键帧（IDR），保证观看端可解码锚点必达 |
| video-data | 不可靠 datagram | delta 帧（'V' 头：seq + codec + data，单报文 ≤1200B 分片） |

关键帧走可靠流的原因：观看端错过 delta 不可怕（下一帧可能又不依赖它），错过关键帧
则必须立刻重传，否则黑屏到下个 IDR（5s）。

## 帧发送策略（控制端 → 受控端反向同构）

- 最新帧信箱语义不变：发送侧永远只拿最新一帧。
- delta 帧 datagram 化：单帧 > PMTU 时按 NAL 分片（H.264），每片带
  (frame_seq, frag_idx, frag_total)；接收端收齐才解码，缺片则丢整帧并统计，
  连续丢 2 帧触发 `RemoteUseCodec` 级别的「请求关键帧」消息（可靠流）。
- JPEG 模式：每帧独立可解，直接全部 datagram 化，丢帧即跳过。

## 自适应

现有发送耗时（send_ms）统计改为「应用层发送排队时间 + QUIC 拥塞窗口水位」；
quinn 暴露 per-connection RTT/丢包估计（`Connection::stats()`），直接喂给现有
分层降质逻辑，替代 Ping/Pong 的部分职责（Ping/Pong 保留用于观看页 RTT 显示）。

## 迁移与开关

- `RemoteUseCodec` 风格的能力协商：认证应答里带 `transports: ["quic","tcp"]`，
  双方都支持则握手后迁到 QUIC（TCP 连接保留为控制兜底，直至 QUIC 首个关键帧到达）。
- 编译期 feature `quic`（默认开），出问题可整包回退 TCP。
- 验证工具：`remote_bench` 增加 `--quic` 模式，对比两套传输在同机/限速链路下的
  fps、端到端帧龄（采集时间戳→显示）、丢帧行为；再做真实 Wi-Fi 抖动测试（关闭
  对端网卡数秒，观察恢复速度与画面连续性）。

## 不做的事

- 不用 QUIC 的 TLS 替代 Noise（保持鉴权/防 MITM 逻辑单一来源）。
- 不做 FEC（QUIC 重传 + 最新帧语义已够；真需要时再加 XOR 前向纠错）。
- 文件回传仍走可靠流（TCP/QUIC stream 均可），无需 datagram。
