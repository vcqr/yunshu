# macOS 授权与签名排障

> 移植自 ashell（同为 Tauri 工程）2026-11 的本地网络授权死锁排查。yunshu 依赖同一类授权：
> 本地网络（UDP 发现 / 传书 / 远程控制）、屏幕录制（xcap 采集）、辅助功能（enigo 注入）。

## 机制

macOS 的隐私类授权（本地网络、屏幕录制、辅助功能…）按「**签名标识 + bundle id**」匹配授权记录，不是只看 bundle id。cargo/ld 默认给二进制打 linker 随机标识（`yunshu-<16 位 hex>`，每次构建必变），于是形成死锁：

1. 旧构建授权过 → 授权存储里 bundle id 已「允许」
2. 新构建标识随机 → 按标识查无记录，内核持续拒绝
3. 系统因 bundle id「已允许」不再弹窗 → 用户无法重新授权

## yunshu 的处理

| 场景 | 做法 |
|---|---|
| CI / DMG | `tauri.conf.json` 里 `bundle.macOS.signingIdentity: "-"`：ad-hoc 签名，标识取 Info.plist 的 `CFBundleIdentifier`（`com.yunshu.desktop`），跨构建稳定；CI 有校验步骤，标识漂移即失败 |
| yunshu-cli（裸二进制） | CI 打包前 `codesign --force --sign - --identifier com.yunshu.cli` |
| 本地开发 | 重编译后跑 `scripts/codesign-macos.sh [debug\|release]` |
| 本地网络弹窗文案 | `crates/yunshu-app/Info.plist` 补 `NSLocalNetworkUsageDescription`（Tauri 默认 Info.plist 没有此键） |

## 排查链（同类问题直接用）

```bash
# 1. 看产物的签名标识：Identifier 应为固定值；flags 含 linker-signed 即随机标识（问题根源）
codesign -dvv target/release/yunshu

# 2. 看本地网络归因与弹窗决策（问题复现后立刻抓，--last 按需调整）
/usr/bin/log show --last 30m --predicate 'composedMessage CONTAINS[c] "LocalNetwork"'

# 3. 看授权存储里实际绑定的签名标识（ashell 死锁即在此定位）
sudo defaults export /Library/Preferences/com.apple.networkextension -

# 4. TCC 类授权（屏幕录制/辅助功能）整类重置后重新弹窗授权
sudo tccutil reset ScreenCapture com.yunshu.desktop
sudo tccutil reset Accessibility com.yunshu.desktop
```

## 更进一步（正式分发时）

ad-hoc 签名的授权绑定是按机器本地的标识记录；换 **Apple Development 证书**签名（`codesign --sign "Apple Development: …"`）后授权绑定「证书 + bundle id」，重构建、分发给别人都不丢授权。CI 配 `APPLE_CERTIFICATE` / `APPLE_CERTIFICATE_PASSWORD` 环境变量即可，tauri bundler 原生支持（未配置时会回退到上面的 ad-hoc 方案）。
