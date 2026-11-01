#!/usr/bin/env bash
# 固定本地构建产物的 ad-hoc 签名标识，让 TCC 授权跨构建保留。
#
# 背景：cargo/ld 默认给二进制打 linker 随机标识（如 yunshu-19d940bf972840ca，
# 每次构建必变）。macOS 的本地网络/屏幕录制/辅助功能等授权按「签名标识 +
# bundle id」匹配记录——标识一变，旧授权查无记录，而系统又因 bundle id
# 「已允许」不再弹窗，形成死锁。用固定标识重签后即可复用已放行记录。
#
# 用法：scripts/codesign-macos.sh [debug|release|<target 子目录>]（默认 release）
# 示例：scripts/codesign-macos.sh debug
#       scripts/codesign-macos.sh aarch64-apple-darwin/release
set -euo pipefail

dir="${1:-release}"
case "$dir" in
  debug | release) dir="target/$dir" ;;
esac

sign() {
  local bin="$1" id="$2"
  if [ ! -f "$bin" ]; then
    echo "跳过（不存在）: $bin"
    return 0
  fi
  codesign --force --sign - --identifier "$id" "$bin"
  echo "已签名: $bin -> $id"
}

sign "$dir/yunshu" com.yunshu.desktop
sign "$dir/yunshu-cli" com.yunshu.cli
