#!/usr/bin/env python3
"""yunshu-cli MCP stdio 冒烟测试：initialize → tools/list → 工具调用（含错误路径）。

用法：
  python3 .agents/skills/yunshu-cli/scripts/mcp_smoke.py [--bin target/release/yunshu-cli]

输出 MCP-TEST-OK 表示全通过；进程退出码 0/1。
"""
import argparse
import json
import socket
import subprocess
import time


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/yunshu-cli")
    ap.add_argument("--home", default="/tmp/yunshu-mcp-smoke")
    args = ap.parse_args()

    proc = subprocess.Popen(
        [args.bin, "--mcp", "--home", args.home, "--port", str(free_port()),
         "--no-dnd", "--name", "mcp冒烟"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, bufsize=1)

    def send(obj):
        proc.stdin.write(json.dumps(obj) + "\n")
        proc.stdin.flush()

    def recv_until(cond, timeout=20):
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = proc.stdout.readline()
            if not line:
                raise RuntimeError("stdout closed（进程退出？跑一次带 stderr 看日志）")
            msg = json.loads(line)
            if cond(msg):
                return msg
        raise TimeoutError

    rpc_id = 0

    def request(method, params=None, timeout=20):
        nonlocal rpc_id
        rpc_id += 1
        msg = {"jsonrpc": "2.0", "id": rpc_id, "method": method}
        if params is not None:
            msg["params"] = params
        send(msg)
        return recv_until(lambda m: m.get("id") == rpc_id and ("result" in m or "error" in m), timeout)

    try:
        r = request("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "0"},
        })
        info = r["result"]["serverInfo"]
        assert info["name"] == "yunshu-cli", info
        print("serverInfo ✓", info)

        send({"jsonrpc": "2.0", "method": "notifications/initialized"})

        r = request("tools/list")
        names = sorted(t["name"] for t in r["result"]["tools"])
        assert names == ["pigeon_list_peers", "pigeon_send_files", "pigeon_send_text"], names
        print("tools ✓", names)

        r = request("tools/call", {"name": "pigeon_list_peers", "arguments": {}})
        assert not r["result"].get("isError"), r
        print("list_peers ✓", r["result"]["content"][0]["text"].strip())

        r = request("tools/call", {"name": "pigeon_send_text",
                                   "arguments": {"peer_id": "zzzz", "text": "hi"}})
        assert r["result"].get("isError") is True, r
        print("send_text 错误路径 ✓")

        r = request("tools/call", {"name": "pigeon_send_files",
                                   "arguments": {"peer_id": "x", "paths": []}})
        assert r["result"].get("isError") is True, r
        print("send_files 空路径 ✓")

        print("MCP-TEST-OK")
    finally:
        proc.terminate()
        proc.wait()


if __name__ == "__main__":
    main()
