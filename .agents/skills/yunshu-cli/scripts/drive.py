#!/usr/bin/env python3
"""驱动 yunshu-cli 交互会话的步骤执行器。

用法（步骤连排，含空格的步骤整体加引号）：
  scripts/drive.py --home /tmp/ys-test --no-dnd -- \
    sleep:3 "send:sendfile <peer_id> /path/to/file" wait:完成:120 send:quit

步骤格式（前缀自分隔，无需分隔符）：
  send:<行>     向 CLI stdin 写一行命令
  wait:<正则>[:超时秒]   等 stdout 出现匹配（默认 30 秒），超时则以非零退出
  sleep:<秒>    睡眠（等发现/传输等异步事件）

CLI 的 stdout（事件回显）与 stderr（tracing 日志）合并透传，便于人工核对。
"""
import argparse
import re
import socket
import subprocess
import sys
import threading
import time


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", default="target/release/yunshu-cli")
    ap.add_argument("--home", required=True, help="配置目录（建议 /tmp 下临时目录，隔离身份）")
    ap.add_argument("--port", type=int, default=None, help="HTTP/WS 端口（默认随机空闲端口）")
    ap.add_argument("--name", default="agent节点")
    ap.add_argument("--no-dnd", action="store_true", help="关防打扰（首次信任新设备前需要）")
    ap.add_argument("--stealth", action="store_true")
    ap.add_argument("steps", nargs="*", help="send:/wait:/sleep: 步骤序列")
    args = ap.parse_args()

    port = args.port or free_port()
    cmd = [args.bin, "--home", args.home, "--port", str(port), "--name", args.name]
    if args.no_dnd:
        cmd.append("--no-dnd")
    if args.stealth:
        cmd.append("--stealth")

    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True, bufsize=1)
    lines: list[str] = []

    def reader() -> None:
        for line in proc.stdout:
            line = line.rstrip("\n")
            print(line, flush=True)
            lines.append(line)

    threading.Thread(target=reader, daemon=True).start()

    ok = True
    try:
        for step in args.steps:
            if step == "--":  # 容忍误写的分隔符
                continue
            if step.startswith("send:"):
                body = step[5:]
                print(f"### >>> {body}", file=sys.stderr, flush=True)
                proc.stdin.write(body + "\n")
                proc.stdin.flush()
            elif step.startswith("wait:"):
                parts = step[5:].split(":", 1)
                pat = parts[0]
                timeout = float(parts[1]) if len(parts) > 1 else 30.0
                rx = re.compile(pat)
                deadline = time.time() + timeout
                while time.time() < deadline:
                    if any(rx.search(l) for l in list(lines)):
                        break
                    time.sleep(0.2)
                else:
                    print(f"### ✗ 等待超时（{timeout}s）：/{pat}/", file=sys.stderr, flush=True)
                    ok = False
                    break
            elif step.startswith("sleep:"):
                time.sleep(float(step[6:]))
            else:
                print(f"### ✗ 未知步骤：{step}", file=sys.stderr, flush=True)
                ok = False
                break
    finally:
        time.sleep(1)  # 给收尾日志留时间
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
