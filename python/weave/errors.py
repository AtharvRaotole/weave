"""Exceptions and error text that cross from host tools into scripts.

A host tool's exception text can contain secrets (paths, tokens, SQL, user
data). By default only the exception *type* reaches the script, its trace and
the model. Raise :class:`ToolError` when the message is safe and useful for the
model to read, or construct the engine with ``debug=True`` to see everything
while developing.
"""

from __future__ import annotations

import traceback


class ToolError(Exception):
    """A tool failure whose message is safe to show to the script and model."""


REDACTED_HINT = "details redacted; raise weave.ToolError for a model-visible message, or use debug=True"


def describe_exception(exc: BaseException, *, debug: bool = False) -> str:
    """Text for a host exception, redacted unless it is a ToolError or debug is on."""
    if debug:
        return "".join(traceback.format_exception(type(exc), exc, exc.__traceback__)).rstrip()
    if isinstance(exc, ToolError):
        return str(exc)
    return f"{type(exc).__name__} ({REDACTED_HINT})"


def call_mismatch(func, args, kwargs) -> str | None:
    """Why a script's call does not fit the tool's signature, or None.

    Only the script's own call shape is described (parameter names, never
    host data), so this is safe to show the model even when the tool's
    exception text is redacted."""
    import inspect

    try:
        signature = inspect.signature(func)
    except (TypeError, ValueError):
        return None
    try:
        signature.bind(*args, **kwargs)
    except TypeError as exc:
        params = []
        for p in signature.parameters.values():
            if p.name == "cancel" or p.kind in (p.VAR_POSITIONAL, p.VAR_KEYWORD):
                continue
            params.append(f"{p.name}=..." if p.kind is p.KEYWORD_ONLY else p.name)
        name = getattr(func, "__name__", "tool")
        return f"bad call: {exc}; call it as {name}({', '.join(params)})"
    return None
