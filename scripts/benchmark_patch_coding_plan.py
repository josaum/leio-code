"""Run the shared bounded editor through Claude Code on the Z.ai Coding Plan."""
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
import shutil
import subprocess
import sys
import tempfile
from threading import Thread
import urllib.error
import urllib.request

import benchmark_patch_completion as bench

MODEL = "glm-5.3-flash"
URL = "https://api.z.ai/api/anthropic"
CLAUDE = shutil.which("claude")


def infer(key, messages, output_limit, remaining, timeout):
    result = {}
    upstream = urllib.request.build_opener(bench.PinnedEndpointRedirectHandler)

    class Gate(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, status, body):
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def reject(self, detail):
            self.reply(400, json.dumps({"type": "error", "error": {
                "type": "invalid_request_error", "message": detail}}).encode())

        def do_POST(self):
            if self.path.partition("?")[0] != "/v1/messages" or result.get("forwarded"):
                return self.reject("Only one model request is allowed per action.")
            size = int(self.headers["Content-Length"])
            if size + 512 + 80 > remaining:
                result["exhausted"] = True
                return self.reject("Cumulative token budget exhausted.")
            request = json.loads(self.rfile.read(size))
            if request.get("model") != MODEL or request.get("tools"):
                return self.reject("Unexpected model or tools.")
            cap = min(output_limit, remaining - size - 512)
            request.update(max_tokens=cap, stream=False)
            result.update(forwarded=True, input_reservation=size + 512, cap=cap)
            headers = {name: self.headers[name] for name in ("anthropic-version", "anthropic-beta")
                       if name in self.headers}
            headers.update({"Authorization": "Bearer " + key, "Content-Type": "application/json"})
            req = urllib.request.Request(URL + "/v1/messages", json.dumps(request).encode(), headers)
            try:
                with upstream.open(req, timeout=timeout) as response:
                    raw = response.read()
                    result["response"] = json.loads(raw)
                # The CLI requests streaming. Translate the single upstream response
                # to the corresponding Anthropic message events.
                if not self.headers.get("Accept", "").startswith("text/event-stream"):
                    return self.reply(200, raw)
                self.stream_result(result["response"])
            except urllib.error.HTTPError as exc:
                result["http_error"] = exc.code
                self.reply(exc.code, exc.read())
            except (TimeoutError, urllib.error.URLError):
                result["uncertain"] = True
                self.reject("Upstream request failed before usage was received.")

        def stream_result(self, response):
            events = [{"type": "message_start", "message": {**response, "content": [], "stop_reason": None}}]
            for index, block in enumerate(response["content"]):
                events.extend([{"type": "content_block_start", "index": index, "content_block": block},
                               {"type": "content_block_stop", "index": index}])
            events.extend([{"type": "message_delta", "delta": {"stop_reason": response["stop_reason"]},
                            "usage": response["usage"]}, {"type": "message_stop"}])
            data = "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    with tempfile.TemporaryDirectory() as scratch, HTTPServer(("127.0.0.1", 0), Gate) as server:
        server.timeout = 1
        worker = Thread(target=server.serve_forever, daemon=True)
        worker.start()
        env = {
            "PATH": os.environ["PATH"], "HOME": scratch, "CLAUDE_CONFIG_DIR": scratch,
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
            "ANTHROPIC_AUTH_TOKEN": "local-benchmark-gate",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        }
        command = [CLAUDE, "--bare", "-p", "--output-format", "json", "--model", MODEL,
                   "--effort", "low", "--max-turns", "1", "--system-prompt", messages[0]["content"],
                   "--strict-mcp-config", "--tools", "", "--no-session-persistence"]
        prompt = "\n\n".join(
            m["content"] if i == 0 else f"{m['role']}:\n{m['content']}"
            for i, m in enumerate(messages[1:])
        )
        try:
            subprocess.run(command, input=prompt, text=True, capture_output=True,
                                 cwd=scratch, env=env, timeout=timeout)
        except subprocess.TimeoutExpired:
            raise TimeoutError("Claude Code timed out.") from None
        finally:
            server.shutdown()
    if result.get("exhausted"):
        return "", None
    if "response" not in result:
        raise RuntimeError(f"Claude Code produced no upstream response. HTTP status {result.get('http_error')}.")
    response = result["response"]
    if response["model"] != MODEL:
        raise RuntimeError("Coding Plan returned a different model.")
    usage = response["usage"]
    total_input = sum(usage.get(k, 0) or 0 for k in (
        "input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))
    output = usage["output_tokens"]
    if total_input > result["input_reservation"] or output > result["cap"]:
        raise RuntimeError("Provider usage exceeded the request reservation.")
    answer = "".join(b["text"] for b in response["content"] if b["type"] == "text")
    return answer, {
        "provider": "Z.ai Coding Plan", "model": MODEL, "tokens": total_input + output,
        "completion_tokens": output, "finish_reason": response["stop_reason"],
        "input_reservation": result["input_reservation"], "output_cap": result["cap"],
        "remaining_before_call": remaining,
    }


def main():
    key = sys.stdin.readline().strip()
    if not key:
        raise RuntimeError("Coding Plan credential required on stdin.")
    solve = bench.solve

    def solve_with_plan(root, case, config, arm, binary):
        config = {**config, "inference": lambda *args: infer(key, *args)}
        return solve(root, case, config, arm, binary)

    bench.solve = solve_with_plan
    bench.main()


if __name__ == "__main__":
    main()
