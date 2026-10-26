#!/usr/bin/env python3
"""Exercise the actual stdio MCP and save one synthetic dashboard capture."""

import base64
import json
import os
from pathlib import Path
import queue
import re
import struct
import subprocess
import sys
import tempfile
import threading


HERE = Path(__file__).resolve().parent
TIMEOUT = 120


def text_content(result):
    return "\n".join(item.get("text", "") for item in result.get("content", []))


def evaluated_json(result):
    text = text_content(result)
    match = re.search(r"```(?:json)?\s*\n(.*?)\n```", text, re.DOTALL)
    if not match:
        raise RuntimeError(f"unexpected evaluate_script response: {text}")
    return json.loads(match.group(1))


def main():
    proc = subprocess.Popen(
        [str(HERE / "mcp.sh")], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        text=True, bufsize=1,
    )
    messages = queue.Queue()

    def read_messages():
        for line in proc.stdout:
            try:
                messages.put(json.loads(line))
            except json.JSONDecodeError:
                messages.put(RuntimeError(f"invalid MCP stdout: {line[:500]!r}"))
        messages.put(RuntimeError("MCP stdout closed"))

    threading.Thread(target=read_messages, daemon=True).start()
    next_id = 0

    def send(method, params=None, notification=False):
        nonlocal next_id
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        if not notification:
            next_id += 1
            message["id"] = next_id
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()
        if notification:
            return None
        while True:
            try:
                response = messages.get(timeout=TIMEOUT)
            except queue.Empty as exc:
                raise RuntimeError(f"MCP timeout waiting for {method}") from exc
            if isinstance(response, Exception):
                raise response
            if response.get("id") != next_id:
                continue
            if "error" in response:
                raise RuntimeError(f"{method}: {response['error']}")
            return response["result"]

    def call(name, arguments):
        result = send("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            raise RuntimeError(f"{name}: {text_content(result)}")
        return result

    try:
        initialized = send("initialize", {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "openproxy-ui-smoke", "version": "1.0.0"},
        })
        send("notifications/initialized", notification=True)
        print(f"MCP initialized: {initialized['serverInfo']['name']} "
              f"(protocol {initialized['protocolVersion']})", flush=True)
        catalog = send("tools/list")
        names = {tool["name"] for tool in catalog["tools"]}
        required = {"new_page", "list_pages", "navigate_page", "evaluate_script",
                    "wait_for", "take_screenshot"}
        if missing := required - names:
            raise RuntimeError(f"missing MCP tools: {sorted(missing)}")
        print(f"MCP catalog: {len(names)} tools, required tools present", flush=True)

        call("new_page", {"url": "https://app:4623/login", "timeout": 30000})
        pages = text_content(call("list_pages", {}))
        selected = re.search(r"(?m)^\s*(\d+):.*\[selected\]", pages)
        if not selected:
            raise RuntimeError(f"could not identify selected page: {pages}")
        page_id = int(selected.group(1))
        call("wait_for", {"pageId": page_id, "text": ["OpenProxy"], "timeout": 30000})
        login = evaluated_json(call("evaluate_script", {
            "pageId": page_id,
            "function": "async () => { const r = await fetch('/api/auth/login', {method: 'POST', "
                        "headers: {'Content-Type': 'application/json'}, "
                        "body: JSON.stringify({password: 'synthetic-browser-only'})}); "
                        "return {status: r.status, body: await r.json()}; }",
        }))
        if login.get("status") != 200 or not login.get("body", {}).get("success"):
            raise RuntimeError(f"synthetic login failed: {login}")

        call("navigate_page", {"pageId": page_id, "type": "url",
                               "url": "https://app:4623/dashboard", "timeout": 30000})
        call("wait_for", {"pageId": page_id, "text": ["Providers"], "timeout": 30000})
        page = evaluated_json(call("evaluate_script", {
            "pageId": page_id,
            "function": "async () => { await document.fonts.ready; "
                        "const names = ['400 14px Inter', '700 14px Inter', "
                        "'400 14px \"Instrument Serif\"', '400 14px \"JetBrains Mono\"', "
                        "'600 14px \"JetBrains Mono\"', '400 14px \"Material Symbols Outlined\"']; "
                        "const fonts = await Promise.all(names.map(name => document.fonts.load(name, 'OpenProxy'))); "
                        "return {url: location.href, title: document.title, "
                        "width: innerWidth, height: innerHeight, "
                        "fontsReady: fonts.every(items => items.length && items.every(f => f.status === 'loaded')), "
                        "authenticated: (await (await fetch('/api/auth/status')).json()).authenticated}; }",
        }))
        if (not page.get("url", "").startswith("https://app:4623/dashboard")
                or "OpenProxy" not in page.get("title", "")
                or not page.get("authenticated")
                or not page.get("fontsReady")
                or (page.get("width"), page.get("height")) != (1280, 720)):
            raise RuntimeError(f"unexpected dashboard page: {page}")
        print(f"Dashboard: {page['url']}, authenticated={page['authenticated']}, fontsReady={page['fontsReady']}, "
              f"viewport={page['width']}x{page['height']}", flush=True)

        screenshot = call("take_screenshot", {"pageId": page_id, "format": "png"})
        image = next((item for item in screenshot.get("content", [])
                      if item.get("type") == "image" and item.get("mimeType") == "image/png"), None)
        if not image:
            raise RuntimeError(f"no PNG returned: {text_content(screenshot)}")
        data = base64.b64decode(image["data"], validate=True)
        if data[:8] != b"\x89PNG\r\n\x1a\n" or len(data) < 24:
            raise RuntimeError("invalid PNG returned")
        dimensions = struct.unpack(">II", data[16:24])
        if dimensions != (1280, 720):
            raise RuntimeError(f"unexpected PNG dimensions: {dimensions}")
        artifacts = HERE / "artifacts"
        artifacts.mkdir(exist_ok=True)
        fd, filename = tempfile.mkstemp(prefix="smoke-", suffix=".png", dir=artifacts)
        with os.fdopen(fd, "wb") as output:
            output.write(data)
        print(f"Screenshot: {filename} ({dimensions[0]}x{dimensions[1]}, {len(data)} bytes)")
    finally:
        if proc.stdin:
            proc.stdin.close()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        sys.exit(1)
