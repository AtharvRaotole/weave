"""Planted-secret tests: host exception text stays out of scripts, traces and
model-facing output unless the host opts in. Offline. Run directly."""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))

import weave  # noqa: E402

SECRET = "sk-PLANTED-0123456789abcdef"


def leaky_read(key):
    raise RuntimeError(f"connect postgres://admin:{SECRET}@db/prod failed for {key}")


def leaky_cooperative(key, cancel):
    raise PermissionError(f"token {SECRET} rejected")


def public_failure(key):
    raise weave.ToolError(f"no record named {key}")


def engine(debug=False):
    e = weave.Engine(debug=debug)
    e.tool("leaky_read", leaky_read, effect="read", arity=1)
    e.tool("leaky_cooperative", leaky_cooperative, effect="read", arity=1)
    e.tool("public_failure", public_failure, effect="read", arity=1)
    return e


def failure_text(e, source):
    try:
        e.run(source)
    except ValueError as exc:
        return str(exc)
    raise AssertionError("expected failure")


def test_uncaught_errors_redact_host_text():
    e = engine()
    for tool in ("leaky_read", "leaky_cooperative"):
        text = failure_text(e, f'{tool}("k")\n')
        assert SECRET not in text and "postgres" not in text, text
        assert "redacted" in text and ("RuntimeError" in text or "PermissionError" in text), text


def test_caught_errors_and_traces_do_not_retain_secret():
    e = engine()
    src = 'try:\n    x = leaky_read("k")\nexcept ToolError as err:\n    x = str(err)\nprint(x)\n'
    run = e.run(src)
    assert SECRET not in run.stdout and SECRET not in run.trace_json(), run.stdout
    assert "RuntimeError" in run.stdout


def test_tool_error_message_is_model_visible():
    text = failure_text(engine(), 'public_failure("alpha")\n')
    assert "no record named alpha" in text and "redacted" not in text, text


def test_debug_engine_shows_full_text():
    text = failure_text(engine(debug=True), 'leaky_read("k")\n')
    assert SECRET in text and "Traceback" in text, text


def test_weave_tool_output_is_redacted():
    tool = weave.WeaveTool([weave.declare(effect="read", arity=1)(leaky_read)])
    out = tool.run('print(leaky_read("k"))\n')
    assert out.startswith("error:") and SECRET not in out, out
    debug = weave.WeaveTool([weave.declare(effect="read", arity=1)(leaky_read)], debug=True)
    assert SECRET in debug.run('print(leaky_read("k"))\n')


def test_isolated_worker_errors_are_redacted():
    from isolation_tools import secret_failure
    e = weave.Engine(worker_pool=1, isolate_all=True)
    e.tool("secret_failure", secret_failure, effect="read")
    text = failure_text(e, f'secret_failure("{SECRET}")\n')
    assert SECRET not in text, text


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for test in tests:
        test()
        print("ok", test.__name__, flush=True)
    print(f"{len(tests)}/{len(tests)} privacy tests passed")


if __name__ == "__main__":
    main()
