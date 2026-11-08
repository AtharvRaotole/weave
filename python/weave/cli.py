"""`weave check`, `run`, `replay`, and `trace`.

Scripted tools are `echo` (return the positional argument) or `const` (return
a fixed string). Real host functions are registered in Python with
`Engine.tool` or `@weave.declare`.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

HELP = """\
weave check FILE [--tool name=...,effect=read,arity=1]
weave run FILE [--tool name=...,effect=read,behavior=echo] [--trace-out PATH]
weave replay FILE --trace PATH [--tool ...]
weave trace PATH

--tool fields: name, effect (pure|read|reversible|irreversible), arity,
behavior (echo|const), result (a string, required for const).
echo returns its single positional argument.
"""


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    try:
        text = dispatch(args)
    except Exception as exc:
        print(f"weave: {exc}", file=sys.stderr)
        return 1
    if text:
        sys.stdout.write(text if text.endswith("\n") else text + "\n")
    return 0


def dispatch(args: list[str]) -> str:
    if not args or args[0] in ("-h", "--help"):
        return HELP
    cmd, rest = args[0], args[1:]
    if cmd == "check":
        return cmd_check(rest)
    if cmd == "run":
        return cmd_run(rest)
    if cmd == "replay":
        return cmd_replay(rest)
    if cmd == "trace":
        return cmd_trace(rest)
        raise RuntimeError(f"unknown command `{cmd}`\n{HELP}")


def parse_flags(args: list[str], need_trace: bool) -> dict:
    file = None
    trace = None
    trace_out = None
    tools = []
    i = 0
    while i < len(args):
        arg = args[i]
        if arg == "--tool":
            tools.append(parse_tool(args[i + 1]))
            i += 2
        elif arg == "--trace":
            trace = args[i + 1]
            i += 2
        elif arg == "--trace-out":
            trace_out = args[i + 1]
            i += 2
        elif arg.startswith("-"):
            raise RuntimeError(f"unknown flag `{arg}`")
        elif file is None:
            file = arg
            i += 1
        else:
            raise RuntimeError(f"unexpected argument `{arg}`")
    if file is None:
        raise RuntimeError("expected a script path")
    if need_trace and trace is None:
        raise RuntimeError("--trace is required")
    return {"file": file, "trace": trace, "trace_out": trace_out, "tools": tools}


def parse_tool(spec: str) -> dict:
    fields = {}
    for part in spec.split(","):
        if "=" not in part:
            raise RuntimeError(f"tool field `{part}` must be key=value")
        key, value = part.split("=", 1)
        fields[key] = value
    if "name" not in fields:
        raise RuntimeError("tool spec needs name=")
    behavior = fields.get("behavior", "echo")
    if behavior not in ("echo", "const"):
        raise RuntimeError(f"unknown behavior `{behavior}`; use echo or const")
    if behavior == "const" and "result" not in fields:
        raise RuntimeError(f"tool `{fields['name']}` behavior const needs result=")
    return fields


def engine_from(tools: list[dict]):
    from weave import Engine

    engine = Engine()
    for tool in tools:
        name = tool["name"]
        effect = tool.get("effect", "irreversible")
        arity = int(tool["arity"]) if "arity" in tool else None
        behavior = tool.get("behavior", "echo")
        result = tool.get("result")

        def call(*args, behavior=behavior, result=result):
            if behavior == "const":
                return result
            if len(args) == 1:
                return args[0]
            return list(args)

        engine.tool(name, call, effect=effect, arity=arity)
    return engine


def cmd_check(args: list[str]) -> str:
    flags = parse_flags(args, False)
    source = Path(flags["file"]).read_text()
    diags = engine_from(flags["tools"]).check(source)
    if diags:
        raise RuntimeError("\n".join(diags))
    return "ok\n"


def cmd_run(args: list[str]) -> str:
    flags = parse_flags(args, False)
    source = Path(flags["file"]).read_text()
    result = engine_from(flags["tools"]).run(source)
    if flags["trace_out"]:
        Path(flags["trace_out"]).write_text(result.trace_json())
    return result.stdout


def cmd_replay(args: list[str]) -> str:
    flags = parse_flags(args, True)
    source = Path(flags["file"]).read_text()
    trace = Path(flags["trace"]).read_text()
    result = engine_from(flags["tools"]).replay(source, trace)
    return result.stdout


def cmd_trace(args: list[str]) -> str:
    if len(args) != 1:
        raise RuntimeError("trace takes one path")
    trace = json.loads(Path(args[0]).read_text())
    lines = ["seq\tline\ttool\teffect\targs\tresult\terror"]
    for event in trace.get("events", []):
        if event.get("error"):
            shown = ""
            error = event["error"]
        else:
            shown = _plain(event.get("result"))
            error = ""
        lines.append(
            "\t".join(
                [
                    str(event.get("seq", "")),
                    str(event.get("line", "")),
                    str(event.get("tool", "")),
                    str(event.get("effect", "")),
                    _plain(event.get("args")),
                    shown,
                    error,
                ]
            )
        )
    return "\n".join(lines) + "\n"


def _plain(value) -> str:
    if isinstance(value, dict) and "t" in value:
        tag = value["t"]
        if tag == "none":
            return "null"
        if tag == "list":
            return "[" + ", ".join(_plain(item) for item in value.get("v", [])) + "]"
        if tag == "dict":
            parts = [f"{_plain(k)}: {_plain(v)}" for k, v in value.get("v", [])]
            return "{" + ", ".join(parts) + "}"
        return str(value.get("v"))
    if isinstance(value, list):
        return "[" + ", ".join(_plain(item) for item in value) + "]"
    return str(value)


if __name__ == "__main__":
    raise SystemExit(main())
