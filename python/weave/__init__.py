"""Weave Python package. The runtime is the native extension."""

from weave._native import CancelToken, Engine, Run, Session, Speculation, declare
from weave.agent_tool import WeaveTool

try:
    from importlib.metadata import PackageNotFoundError, version

    __version__ = version("weave")
except PackageNotFoundError:  # running from a source tree without an install
    __version__ = "0.0.0"

__all__ = [
    "CancelToken",
    "Engine",
    "Run",
    "Session",
    "Speculation",
    "WeaveTool",
    "__version__",
    "declare",
]
