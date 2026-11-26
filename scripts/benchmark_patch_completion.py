#!/usr/bin/env python3
"""Paired public-repository patch pilot. Requires macOS sandbox-exec, pytest and the case test dependencies."""

import argparse
import ast
import math
import re
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from difflib import unified_diff


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args])


def snapshot(repo, rev, root):
    root.mkdir()
    archive = git(repo, "archive", rev)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(root, filter="data")


def grade(root, repo, case):
    test = case["test_file"]
    (root / test).write_bytes(git(repo, "show", f"{case['fix']}:{test}"))
    return run_tests(root, test)


def run_tests(root, test, *, timeout=100):
    scratch = root / ".grader"
    scratch.mkdir(exist_ok=True)
    q = lambda p: json.dumps(str(Path(p).resolve()))
    reads = [
        root,
        sys.prefix,
        sys.base_prefix,
        "/System",
        "/usr",
        "/Library/Apple",
        "/opt/homebrew",
        "/dev",
        "/private/etc",
    ]
    profile = (
        "(version 1)(deny default)(allow process*)(allow sysctl-read)(allow mach-lookup)"
        '(allow file-read-metadata)(allow file-read* (literal "/"))(allow file-read* '
        + " ".join("(subpath " + q(p) + ")" for p in reads)
        + ")"
        "(allow file-write* (subpath " + q(scratch) + ') (literal "/dev/null"))'
    )
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(scratch),
        "TMPDIR": str(scratch),
        "PYTHONPATH": str(root / "src"),
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTEST_DISABLE_PLUGIN_AUTOLOAD": "1",
        "HYPOTHESIS_STORAGE_DIRECTORY": str(scratch / "hypothesis"),
    }
    run = subprocess.run(
        [
            "/usr/bin/sandbox-exec",
            "-p",
            profile,
            sys.executable,
            "-m",
            "pytest",
            "-q",
            "-o",
            "addopts=",
            "-p",
            "no:cacheprovider",
            test,
        ],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return {
        "passed": run.returncode == 0,
        "exit": run.returncode,
        "output": (run.stdout + run.stderr)[-2500:],
    }


def view_files(root):
    root = root.resolve()
    return {
        p.relative_to(root).as_posix(): p
        for p in sorted(root.rglob("*"))
        if p.suffix in {".py", ".md", ".rst", ".toml"}
        and p.is_file()
        and not p.is_symlink()
        and p.resolve().is_relative_to(root)
        and not any(s.startswith(".") for s in p.relative_to(root).parts)
    }


def action(request, files, opened, original):
    kind = request["action"]
    if kind == "list":
        return "\n".join(k for k in files if k.startswith(request.get("prefix", "")))[
            :5000
        ]
    if kind == "search":
        term = request["text"].casefold()
        rows = []
        for k, p in files.items():
            if not k.startswith(request.get("prefix", "")):
                continue
            for n, line in enumerate(p.read_text(errors="replace").splitlines(), 1):
                if term in line.casefold():
                    rows.append(f"{k}:{n}: {line.strip()[:160]}")
                if len(rows) == 30:
                    return "\n".join(rows)
        return "\n".join(rows) or "No matches."
    k = request["path"]
    p = files[k]
    data = p.read_text()
    if kind == "read":
        opened.add(k)
        n = max(1, int(request.get("start", 1)))
        lines = data.splitlines()
        end = int(request.get("end", n + 79))
        if end < n:
            return "Read rejected: end must be at least start."
        return "\n".join(
            f"{i + 1}: {lines[i]}" for i in range(n - 1, min(len(lines), n + 79, end))
        )[:8000]
    if kind == "replace" and k.startswith("src/") and p.suffix == ".py":
        old, new = request["old"], request["new"]
        if not old or data.count(old) != 1:
            return "Edit rejected: old must match exactly once."
        original.setdefault(k, data)
        p.write_text(data.replace(old, new, 1))
        return "Applied."
    return "Unsupported action."


INSTRUCTIONS = """Fix the issue with a minimal production edit. You can inspect and edit only the supplied snapshot. Reply with one JSON object. Actions:
{"action":"list","prefix":"src/"}
{"action":"search","text":"literal substring","prefix":"src/"}
{"action":"read","path":"src/example.py","start":1} (optional inclusive end, up to 80 numbered lines)
{"action":"replace","path":"src/example.py","old":"exact unique text","new":"replacement"}
{"action":"done"}
Apply the edit through replace before done. Separate tests will score the patch. No internet or shell tools."""


def parent_test_examples(files, issue, searches):
    """Find usage examples by identifier overlap in the visible parent snapshot."""
    tokens = set(
        re.findall(r"[a-z_][a-z_0-9]{2,}", issue.lower() + " " + " ".join(searches))
    )
    identifiers = {
        part.lower()
        for token in re.findall(r"[A-Za-z_][\w.]*", issue)
        if "_" in token or "." in token.strip(".") or token.isupper()
        for part in token.strip(".").split(".")
        if len(part) >= 3
    }
    candidates = []
    counts = {}
    for path, file in files.items():
        if not file.name.startswith("test_") or file.suffix != ".py":
            continue
        source = file.read_text(errors="replace")
        try:
            tree = ast.parse(source)
        except SyntaxError:
            continue
        lines = source.splitlines()
        for node in ast.walk(tree):
            if not isinstance(
                node, (ast.FunctionDef, ast.AsyncFunctionDef)
            ) or not node.name.startswith("test_"):
                continue
            first = min((d.lineno for d in node.decorator_list), default=node.lineno)
            body = "\n".join(lines[first - 1 : node.end_lineno])
            matches = tokens.intersection(
                re.findall(r"[a-z_][a-z_0-9]{2,}", body.lower())
            )
            candidates.append((path, node.lineno, body, matches))
            for term in matches:
                counts[term] = counts.get(term, 0) + 1
    ranked = sorted(
        (
            (
                len(matches & identifiers),
                sum(
                    math.log1p(len(candidates) / counts[t]) ** 2
                    for t in sorted(matches)
                ),
                path,
                line,
                body,
            )
            for path, line, body, matches in candidates
            if matches
        ),
        key=lambda row: (-row[0], -row[1], row[2], row[3]),
    )
    return "\n\n".join(
        f"{path}:{line}\n{body[:1600]}" for _, _, path, line, body in ranked[:3]
    )


def apply_source_patch(request, files, visible, original):
    """Validate non-overlapping anchors against the inspected preimage, then write once."""
    path = request["path"]
    if path not in visible:
        raise ValueError("Select an inspected production file.")
    raw = files[path].read_bytes().decode()
    edits = request["edits"]
    if not isinstance(edits, list) or not edits:
        raise ValueError("Supply at least one source replacement.")
    spans = []
    lines = raw.splitlines()
    for edit in edits:
        old, new = edit["old"], edit["new"]
        if not isinstance(old, str) or not old or not isinstance(new, str):
            raise ValueError("Use a nonempty old string and a new string.")
        if "\r\n" in raw:
            old = old.replace("\r\n", "\n").replace("\n", "\r\n")
            new = new.replace("\r\n", "\n").replace("\n", "\r\n")
        if raw.count(old) != 1:
            raise ValueError(
                f"Expected exactly one match for {old[:120]!r}; copy an exact unique substring."
            )
        start = raw.index(old)
        end = start + len(old)
        first = raw.count("\n", 0, start) + 1
        last = raw.count("\n", 0, end - 1) + 1
        if any(visible[path].get(n) != lines[n - 1] for n in range(first, last + 1)):
            raise ValueError("The selected source changed or was not fully inspected.")
        spans.append((start, end, new))
    spans.sort()
    previous = len(raw)
    changed = raw
    for start, end, replacement in reversed(spans):
        if end > previous:
            raise ValueError("Source replacements overlap.")
        changed = changed[:start] + replacement + changed[end:]
        previous = start
    if changed == raw:
        raise ValueError("Patch made no change.")
    ast.parse(changed, filename=path)
    files[path].write_bytes(changed.encode())
    original.setdefault(path, raw)
    return "Applied."


def issue_identifiers(issue):
    words = re.findall(r"[A-Za-z_][\w.]*", issue)
    return {
        part.casefold()
        for word in words
        if "_" in word or "." in word.rstrip(".")
        for part in word.strip(".").split(".")
        if len(part) >= 3
    }


def source_excerpts(files, log, anchors):
    """Show executable uses of named identifiers in files found by ordinary navigation."""
    paths = set()
    for request, result in log:
        if request.get("action") == "read":
            paths.add(request["path"])
        elif request.get("action") == "search":
            paths.update(row.partition(":")[0] for row in result.splitlines())
    windows = []
    sources = {}
    for path in sorted(paths):
        if path not in files or not path.startswith("src/") or not path.endswith(".py"):
            continue
        lines = files[path].read_text().splitlines()
        sources[path] = lines
        for i, line in enumerate(lines):
            text = line.strip()
            if not text or text.startswith(("#", ":", "..", '"' * 3, "'" * 3)):
                continue
            matches = anchors.intersection(
                re.findall(r"[a-z_][a-z_0-9]*", text.casefold())
            )
            if not matches:
                continue
            # Branches, returns and calls show behavior. Declarations and prose often
            # explain the name but do not implement the requested change.
            operational = text.startswith(("if ", "elif ", "return ", "for ")) or (
                "=" in text and "(" in text
            )
            if not operational and not any(mark in text for mark in ("=", "(", "[")):
                continue
            windows.append((not operational, -len(matches), path, i))
    if not windows:
        return "", {}
    visible = {}
    size = 0
    # Include the definitions behind calls on matched lines. In particular,
    # metadata helpers can mutate their argument instead of returning a copy.
    for path, lines in sources.items():
        direct_lines = {i for _, _, candidate, i in windows if candidate == path}
        callees = {
            name
            for i in direct_lines
            for name in re.findall(r"\bself\.([A-Za-z_]\w*)\(", lines[i])
        }
        aliases = re.findall(
            r"\bself\.(\w+)\s*=\s*self\.(\w+)\s*$", "\n".join(lines), re.MULTILINE
        )
        callees.update(target for name, target in aliases if name in callees)
        for i, line in enumerate(lines):
            definition = re.match(r"\s*def\s+([A-Za-z_]\w*)\(", line)
            if definition and definition[1] in callees:
                windows.append((False, -2, path, i))
    for _, _, path, i in sorted(windows):
        lines = sources[path]
        known = visible.setdefault(path, {})
        for n in range(max(0, i - 5), min(len(lines), i + 25)):
            if n + 1 not in known and size + len(lines[n]) + 1 <= 6500:
                known[n + 1] = lines[n]
                size += len(lines[n]) + 1
    chunks = []
    for path, lines in visible.items():
        prev = None
        for n, text in sorted(lines.items()):
            if prev is None or n != prev + 1:
                chunks.append(f"\n{path} from line {n}:")
            chunks.append(text)
            prev = n
    return "\n".join(chunks), visible


EDIT_PROMPT = """Implement the issue in the shown source. Preserve the existing API and exact output conventions demonstrated in the parent tests. Trace every requested input through the code, including empty inputs and fallbacks when relevant. Check both reporting and behavior when they use different branches. If you change metadata on an object, check whether it is shared across calls or owners. Verify every requirement against the resulting code before answering DONE. Respond with one or more exact SEARCH/REPLACE blocks. Multiple blocks in the same file are allowed. Copy each SEARCH substring verbatim, including indentation; it must match uniquely. Keep the replacement focused. You may briefly reason about the requirements before the blocks.
Format:
<<< SEARCH src/example.py
original code
===
replacement code
>>> END
If no changes are necessary, answer DONE."""


def parse_blocks(content):
    pattern = r"^<<< SEARCH ([^\r\n]+)\r?\n(.*?)^===\r?\n(.*?)^>>> END[ \t]*\r?$"
    matches = re.findall(pattern, content or "", re.MULTILINE | re.DOTALL)
    if not matches:
        raise ValueError("No complete SEARCH/REPLACE block was returned.")
    paths = {path.strip() for path, _, _ in matches}
    if len(paths) != 1:
        raise ValueError("Submit edits to one inspected file at a time.")
    return {
        "action": "patch",
        "path": paths.pop(),
        "edits": [
            {
                "old": old.removesuffix("\n").removesuffix("\r"),
                "new": new.removesuffix("\n").removesuffix("\r"),
            }
            for _, old, new in matches
        ],
    }


def fit_action_history(messages, remaining):
    """Retain complete recent action/result pairs within the remaining request budget."""
    output_cap = 1800
    packet = list(messages)
    while True:
        # A byte per token is a conservative allowance for the public source snippets.
        allowance = (
            remaining - 512 - sum(len(m["content"].encode("utf-8")) for m in packet)
        )
        if allowance >= output_cap or len(packet) <= 4:
            return packet, min(allowance, output_cap)
        del packet[
            2:4
        ]  # Preserve the issue and the most recent action with its result.


def daily_quota_block(exc):
    if exc.code != 429 or exc.headers.get("X-RateLimit-Remaining") != "0":
        return None
    try:
        message = json.load(exc)["error"]["message"]
    except (ValueError, KeyError, TypeError):
        return None
    if not isinstance(message, str) or "per-day" not in message.casefold():
        return None
    reset = exc.headers.get("X-RateLimit-Reset", "")
    return {
        "http_status": 429,
        "period": "daily",
        "remaining": 0,
        "resets_at": datetime.fromtimestamp(int(reset) / 1000, timezone.utc).isoformat()
        if reset.isdecimal()
        else None,
    }


def provider_quota_block(exc, transport):
    if transport != "xai" or exc.code != 403:
        return daily_quota_block(exc)
    try:
        payload = json.load(exc)
    except ValueError:
        return None
    if not isinstance(payload, dict) or payload.get("code") != "permission-denied":
        return None
    detail = payload.get("error")
    if isinstance(detail, str) and any(
        phrase in detail.casefold()
        for phrase in ("used all available credits", "reached its monthly spending limit")
    ):
        return {"http_status": 403, "reason": "team_credit_or_spending_limit"}
    return None


def inference_request(config, messages, output_budget, *, json_action):
    if config.get("transport") == "xai":
        key = os.environ.get("XAI_API_KEY")
        if not key:
            raise RuntimeError("Direct xAI inference requires XAI_API_KEY.")
        url = "https://api.x.ai/v1/responses"
        body = {
            "model": config["model"],
            "input": messages,
            "store": False,
            "temperature": 0,
            "max_output_tokens": output_budget,
            "reasoning": {"effort": config.get("reasoning_effort", "low")},
        }
        if json_action:
            body["text"] = {"format": {"type": "json_object"}}
    elif config.get("transport") == "openai-compatible":
        url = os.environ["BENCHMARK_API_BASE_URL"].rstrip("/") + "/chat/completions"
        endpoint = urllib.parse.urlsplit(url)
        if (endpoint.scheme != "https" or not endpoint.hostname or endpoint.username is not None
                or endpoint.query or endpoint.fragment or url != config["endpoint"]):
            raise ValueError("The HTTPS endpoint must match the benchmark manifest.")
        key = os.environ["BENCHMARK_API_KEY"]
        body = {
            "model": config["model"], "messages": messages, "temperature": 0,
            "max_tokens": output_budget,
        }
        if json_action:
            body["response_format"] = {"type": "json_object"}
    else:
        key = os.environ["OPENROUTER_API_KEY"]
        url = "https://openrouter.ai/api/v1/chat/completions"
        body = {
            "model": config["model"],
            "messages": messages,
            "temperature": 0,
            "max_tokens": output_budget,
            "reasoning": {"effort": config.get("reasoning_effort", "low")},
            "provider": {"allow_fallbacks": False, "order": [config["provider"]]},
        }
        if json_action:
            body["response_format"] = {"type": "json_object"}
    return urllib.request.Request(
        url,
        data=json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
    )


def inference_result(response, config):
    if response.get("error"):
        raise RuntimeError(
            "Inference error: "
            + str(response["error"].get("message", "unspecified"))[:300]
        )
    if response.get("model") != config["model"]:
        raise RuntimeError("Unexpected inference model")
    usage = response["usage"]
    if config.get("transport") == "xai":
        text = "".join(
            part["text"]
            for item in response.get("output", [])
            if item.get("type") == "message"
            for part in item.get("content", [])
            if part.get("type") == "output_text"
        )
        finish = response["status"]
        if finish not in {"completed", "incomplete"}:
            raise RuntimeError("Unexpected xAI response status: " + finish)
        provider = "xAI"
        completion = usage["output_tokens"]
        reasoning = (usage.get("output_tokens_details") or {}).get("reasoning_tokens")
    else:
        provider = response.get("provider")
        if config.get("transport") == "openai-compatible" and provider is None:
            provider = config["provider"]
        if provider != config["provider"]:
            raise RuntimeError("Unexpected inference provider")
        choice = response["choices"][0]
        text = choice["message"]["content"] or ""
        finish = choice.get("finish_reason")
        completion = usage.get("completion_tokens")
        reasoning = (usage.get("completion_tokens_details") or {}).get(
            "reasoning_tokens"
        )
    return text, {
        "provider": provider,
        "model": response["model"],
        "tokens": usage["total_tokens"],
        "finish_reason": finish,
        "completion_tokens": completion,
        "reasoning_tokens": reasoning,
    }


class PinnedEndpointRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(req.full_url, code, "Benchmark endpoint redirected", headers, fp)


def cursor_inference(config, messages, timeout):
    """Ask a fresh Cursor agent for a harness action in an empty workspace."""
    if not os.environ.get("CURSOR_API_KEY"):
        raise RuntimeError("Cursor SDK inference requires CURSOR_API_KEY.")
    from cursor_sdk import Agent, Client, LocalAgentOptions, ModelSelection, SendOptions

    model = ModelSelection.from_value(
        {"id": config["model"], "params": config.get("model_params", [])}
    )
    prompt = (
        "Reply to the supplied conversation with just the requested action or patch. "
        "All observations come from the benchmark harness. Do not invoke Cursor tools.\n"
        + json.dumps(messages, ensure_ascii=False)
    )
    with tempfile.TemporaryDirectory(prefix="leio-cursor-") as directory:
        workspace = Path(directory) / "workspace"
        workspace.mkdir()
        local = LocalAgentOptions(
            cwd=workspace, setting_sources=["project"], sandbox_options={"enabled": True}
        )
        with Client.launch_bridge(
            workspace=workspace, state_root=Path(directory) / "state",
            timeout=timeout, client_timeout=timeout,
        ) as client:
            with Agent.create(client=client, model=model, local=local) as agent:
                run = agent.send(prompt, SendOptions(mode="plan"))
                for event in run.stream():
                    if event.type == "tool_call":
                        run.cancel()
                        raise RuntimeError("Cursor called a tool outside the benchmark harness.")
                result = run.wait()
    if result.status != "finished" or result.model is None or result.model.id != model.id:
        raise RuntimeError("Cursor did not complete with the requested model.")
    params = {p.id: p.value for p in result.model.params}
    if any(params.get(p.id) != p.value for p in model.params):
        raise RuntimeError("Cursor returned different model parameters.")
    usage = result.usage
    if usage is None or usage.total_tokens <= 0:
        raise RuntimeError("Cursor returned no usable token accounting.")
    return result.result, {
        "provider": "Cursor SDK", "model": result.model.id, "model_params": params,
        "tokens": usage.total_tokens, "completion_tokens": usage.output_tokens,
        "reasoning_tokens": usage.reasoning_tokens, "finish_reason": result.status,
    }


def solve(root, case, config, arm, binary):
    files = view_files(root)
    opened = set()
    original = {}
    log = []
    calls = []
    usage = 0
    issue = case["issue"]
    hint = ""
    if arm == "leio":
        subprocess.run(
            [binary, "--repo", str(root), "index"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )
    start = time.monotonic()
    deadline = start + config["max_seconds_per_task"]
    if arm == "leio":
        packet = json.loads(
            subprocess.check_output(
                [
                    binary,
                    "--repo",
                    str(root),
                    "--json",
                    "context",
                    issue,
                    "--limit",
                    "6",
                ]
            )
        )
        hint = "\nInitial LEIO suggestions:\n" + json.dumps(
            [
                {"path": f["path"], "reasons": f["reasons"][:2]}
                for f in packet["entities"][0]["files_to_read"]
            ]
        )
    blocked = None
    anchors = issue_identifiers(issue) if config.get("grounded_edit", False) else set()
    examples = None
    handoff = False
    feedback = ""
    parent_feedback = ""
    stop = "step limit"
    while len(calls) < config["max_steps"]:
        clock = deadline - time.monotonic()
        if clock <= 0:
            stop = "time limit"
            break
        messages = [
            {"role": "system", "content": INSTRUCTIONS},
            {"role": "user", "content": issue + hint + parent_feedback},
        ]
        for request, observation in log[-6:]:
            messages.append({"role": "assistant", "content": json.dumps(request)})
            messages.append({"role": "user", "content": observation})
        source, visible = (
            source_excerpts(files, log, anchors)
            if anchors and len(calls) >= 2
            else ("", {})
        )
        if source:
            handoff = True
            if examples is None:
                examples = parent_test_examples(files, issue, list(anchors))
            messages = [
                {"role": "system", "content": EDIT_PROMPT},
                {
                    "role": "user",
                    "content": issue
                    + parent_feedback
                    + "\nExisting parent tests for usage and formatting:\n"
                    + examples
                    + "\nCurrent editable source:\n"
                    + source
                    + (
                        "\nFeedback on your last patch:\n" + feedback
                        if feedback
                        else ""
                    ),
                },
            ]
        messages, output_budget = fit_action_history(
            messages, config["max_tokens_per_task"] - usage
        )
        if output_budget < 80:
            stop = "token limit"
            break
        try:
            if "inference" in config:
                content, call = config["inference"](
                    messages, output_budget, config["max_tokens_per_task"] - usage,
                    min(90, clock),
                )
                if call is None:
                    stop = "token limit"
                    break
            elif config.get("transport") == "cursor-sdk":
                content, call = cursor_inference(config, messages, min(90, clock))
            else:
                req = inference_request(
                    config, messages, output_budget, json_action=not bool(source)
                )
                open_request = urllib.request.urlopen
                if config.get("transport") == "openai-compatible":
                    open_request = urllib.request.build_opener(PinnedEndpointRedirectHandler).open
                with open_request(req, timeout=min(90, clock)) as resp:
                    content, call = inference_result(json.load(resp), config)
        except urllib.error.HTTPError as exc:
            blocked = provider_quota_block(exc, config.get("transport"))
            if blocked:
                stop = "provider quota"
                break
            if exc.code in (429, 502, 503, 504) and deadline - time.monotonic() > 5:
                time.sleep(4)
                continue
            raise
        except TimeoutError:
            stop = "request timeout"
            break
        calls.append(call)
        usage += call["tokens"]
        if (usage > config["max_tokens_per_task"] or time.monotonic() > deadline
                or (config.get("transport") == "cursor-sdk" and call["completion_tokens"] > output_budget)):
            stop = "budget overrun"
            break
        try:
            if source:
                if content.strip().upper() == "DONE" and original and not parent_feedback:
                    stop = "done"
                    break
                a = parse_blocks(content)
                result = apply_source_patch(a, files, visible, original)
                feedback = "Applied. Review the current source against all requirements. If complete, answer DONE."
            else:
                a = json.loads(content)
                if not isinstance(a, dict) or not isinstance(a.get("action"), str):
                    raise ValueError(
                        "Return a single JSON object with an action field."
                    )
                if a.get("action") == "done" and not parent_feedback:
                    stop = "done"
                    break
                result = action(a, files, opened, original)
        except (KeyError, ValueError, TypeError, SyntaxError) as e:
            a = {"action": "invalid"}
            result = str(e)
            feedback = result
        if (config.get("validate_parent_tests") and case.get("test_file")
                and a.get("action") in {"replace", "patch"} and result.startswith("Applied")):
            clock = deadline - time.monotonic()
            if clock <= 0:
                stop = "time limit"
                break
            try:
                grade = run_tests(root, case["test_file"], timeout=min(100, clock))
            except subprocess.TimeoutExpired:
                stop = "parent test timeout"
                break
            parent_feedback = "" if grade["passed"] else (
                "\nThe current patch fails existing parent-revision tests. "
                "Correct the regression before finishing.\n" + grade["output"][-1800:]
            )
            calls[-1]["parent_tests_passed"] = grade["passed"]
        if a.get("action") in {"invalid", "replace", "patch"}:
            calls[-1]["action_result"] = result[:300]
        log.append([a, result])
    patch = "".join(
        "".join(
            unified_diff(
                before.splitlines(True),
                (root / k).read_text().splitlines(True),
                fromfile="a/" + k,
                tofile="b/" + k,
            )
        )
        for k, before in original.items()
    )
    return {
        "stop": stop,
        "hint": hint,
        "grounded_edit": handoff,
        "provider_block": blocked,
        "parent_test_examples": examples,
        "tokens": usage,
        "seconds": round(time.monotonic() - start, 2),
        "files_read": sorted(opened),
        "calls": calls,
        "actions": [a for a, _ in log],
        "patch": patch,
    }


def qualify(config, output):
    """Exercise a model edit and grade it outside the retrieval cases."""
    with tempfile.TemporaryDirectory(prefix="leio-edit-control-") as temp:
        root = Path(temp).resolve()
        (root / "src").mkdir()
        (root / "src/units.py").write_text(
            "def minutes_to_seconds(minutes):\n    return minutes + 60\n"
        )
        test = "test_units.py"
        issue = (
            "Fix minutes_to_seconds in src/units.py so it converts any integer "
            "minutes to seconds. Inspect the file, apply the correction and finish."
        )
        rec = solve(root, {"issue": issue}, config, "ordinary", None)
        (root / test).write_text(
            "from units import minutes_to_seconds\n"
            "def test_conversion():\n"
            "    assert [minutes_to_seconds(n) for n in (-3, 0, 2)] == [-180, 0, 120]\n"
        )
        if rec["provider_block"]:
            rec["success"] = None
            (output / "qualification.json").write_text(json.dumps(rec, indent=2) + "\n")
            return False
        rec["grade"] = run_tests(root, test)
        rec["success"] = (
            bool(rec["patch"])
            and rec["grade"]["passed"]
            and rec["stop"] != "budget overrun"
        )
        (output / "qualification.json").write_text(json.dumps(rec, indent=2) + "\n")
        return rec["success"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("manifest", "output"):
        parser.add_argument("--" + key, required=True, type=Path)
    for key in ("clones", "binary"):
        parser.add_argument("--" + key, type=Path)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--controls-only", action="store_true")
    mode.add_argument(
        "--qualify-only",
        action="store_true",
        help="Exercise a known-file edit with the configured model",
    )
    parser.add_argument(
        "--grounded-edit",
        action="store_true",
        help="Evaluate the source-anchored editor after discovering a named identifier",
    )
    parser.add_argument(
        "--case", action="append", help="Run only this manifest repository"
    )
    args = parser.parse_args()
    if not args.qualify_only and args.clones is None:
        parser.error("--clones is required for repository cases")
    if not (args.qualify_only or args.controls_only) and args.binary is None:
        parser.error("--binary is required for paired trials")
    config = json.loads(args.manifest.read_text())
    if args.grounded_edit:
        config["grounded_edit"] = True
    args.output.mkdir(parents=True, exist_ok=True)
    if args.qualify_only:
        raise SystemExit(0 if qualify(config, args.output) else 1)
    for i, case in enumerate(config["cases"]):
        if args.case and case["repo"] not in args.case:
            continue
        repo = args.clones / case["repo"]
        assert (
            git(repo, "rev-parse", case["fix"] + "^").decode().strip() == case["base"]
        )
        with tempfile.TemporaryDirectory(prefix="leio-patch-") as tmp:
            for label in ("base", "fix"):
                r = Path(tmp).resolve() / label
                snapshot(repo, case[label], r)
                g = grade(r, repo, case)
                (args.output / f"{case['repo']}-{label}.json").write_text(
                    json.dumps(g, indent=2) + "\n"
                )
                if g["exit"] != (1 if label == "base" else 0):
                    raise RuntimeError(f"{case['repo']} {label}: {g}")
            if args.controls_only:
                continue
            for arm in ["ordinary", "leio"] if i % 2 == 0 else ["leio", "ordinary"]:
                r = Path(tmp).resolve() / arm
                snapshot(repo, case["base"], r)
                rec = solve(r, case, config, arm, str(args.binary.resolve()))
                receipt = args.output / f"{case['repo']}-{arm}.json"
                rec["base"] = case["base"]
                if rec["provider_block"]:
                    rec["success"] = None
                    receipt.write_text(json.dumps(rec, indent=2) + "\n")
                    raise SystemExit(
                        f"Model trial blocked by provider quota. Receipt: {receipt}"
                    )
                rec["binary_sha256"] = hashlib.sha256(
                    args.binary.read_bytes()
                ).hexdigest()
                receipt.write_text(json.dumps(rec, indent=2) + "\n")
                try:
                    rec["grade"] = grade(r, repo, case)
                except subprocess.TimeoutExpired:
                    rec["grade"] = {
                        "passed": False,
                        "exit": 124,
                        "output": "Upstream tests exceeded the 100 second grading limit",
                    }
                rec["success"] = (
                    bool(rec["patch"])
                    and rec["grade"]["passed"]
                    and rec["stop"] != "budget overrun"
                )
                receipt.write_text(json.dumps(rec, indent=2) + "\n")
                print(case["repo"], arm, rec["success"], rec["tokens"], flush=True)


if __name__ == "__main__":
    main()
