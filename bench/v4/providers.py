"""OpenAI-compatible Chat Completions client used by every provider.

No SDK, no hidden retries: each HTTP attempt is recorded with its usage and
elapsed time. Live requests require the caller to pass ``live=True`` (the
runner sets it only with ``--allow-live``). Keys come from the environment
and are never written anywhere.
"""

from __future__ import annotations

import json
import os
import socket
import threading
import time
import urllib.error
import urllib.request

PROVIDERS = {
    "openai": {"base": "https://api.openai.com/v1", "env": "OPENAI_API_KEY"},
    # Gemini's OpenAI-compatible endpoint.
    "gemini": {"base": "https://generativelanguage.googleapis.com/v1beta/openai", "env": "GEMINI_API_KEY"},
    # GLM through NVIDIA's hosted OpenAI-compatible API (build.nvidia.com key).
    "nvidia": {"base": "https://integrate.api.nvidia.com/v1", "env": "GLM_API_KEY"},
    # GLM direct (Zhipu / Z.ai) if such a key is supplied.
    "zai": {"base": "https://api.z.ai/api/paas/v4", "env": "ZAI_API_KEY"},
}


_PACE_LOCK = threading.Lock()
_PACE_NEXT = [0.0]


def _pace(interval_s):
    """Space requests from every client in this process at least `interval_s` apart (free-tier quotas).
    Returns the seconds slept; the trial records it with provider waits, not model time."""
    if interval_s <= 0:
        return 0.0
    with _PACE_LOCK:
        now = time.monotonic()
        start = max(now, _PACE_NEXT[0])
        _PACE_NEXT[0] = start + interval_s
    if start > now:
        time.sleep(start - now)
    return start - now


class ProviderError(Exception):
    def __init__(self, kind, detail=""):
        super().__init__(f"{kind}: {detail}")
        self.kind = kind


class ChatClient:
    def __init__(self, provider, model, *, live=False, reasoning_effort=None, temperature=None,
                 max_tokens=4096, timeout_s=180, retries=4, request_cap=10_000, min_interval_s=0.0):
        if provider not in PROVIDERS:
            raise ValueError(f"unknown provider {provider}")
        if not live:
            raise ValueError("live requests need an explicit live=True opt-in")
        spec = PROVIDERS[provider]
        self._key = os.environ.get(spec["env"])
        if not self._key:
            raise ValueError(f"{spec['env']} is not set")
        self.provider, self.model, self.base = provider, model, spec["base"]
        self.reasoning_effort, self.temperature, self.max_tokens = reasoning_effort, temperature, max_tokens
        self.timeout_s, self.retries, self.request_cap = timeout_s, retries, request_cap
        self.sent = 0
        self.min_interval_s = min_interval_s

    def payload(self, messages, tools):
        body = {"model": self.model, "messages": [{k: v for k, v in m.items() if k != "_raw"} for m in messages]}
        if tools:
            body["tools"] = tools
            body["parallel_tool_calls"] = True
            if self.provider == "gemini":
                body.pop("parallel_tool_calls")  # accepted implicitly; the flag is rejected
        if self.provider == "openai" and self.reasoning_effort:
            body["max_completion_tokens"] = self.max_tokens
        else:
            body["max_tokens"] = self.max_tokens
        if self.reasoning_effort:
            body["reasoning_effort"] = self.reasoning_effort
        if self.temperature is not None:
            body["temperature"] = self.temperature
        return body

    def responses_payload(self, messages, tools):
        """OpenAI: the Responses API (Chat Completions rejects tools with reasoning on GPT-5.x).

        Earlier output items (including encrypted reasoning) are replayed verbatim from the
        assistant message's ``_raw`` field, so no provider-side state is used (store=false)."""
        items, instructions = [], None
        for m in messages:
            if m["role"] == "system":
                instructions = m["content"]
            elif m["role"] == "user":
                items.append({"role": "user", "content": m["content"]})
            elif m["role"] == "assistant":
                items.extend(m.get("_raw") or [{"role": "assistant", "content": m.get("content") or ""}])
            elif m["role"] == "tool":
                items.append({"type": "function_call_output", "call_id": m["tool_call_id"], "output": m["content"]})
        body = {"model": self.model, "input": items, "store": False, "max_output_tokens": self.max_tokens,
                "parallel_tool_calls": True}
        if instructions:
            body["instructions"] = instructions
        if tools:
            body["tools"] = [{"type": "function", "name": t["function"]["name"],
                              "description": t["function"]["description"],
                              "parameters": t["function"]["parameters"], "strict": False} for t in tools]
        if self.reasoning_effort:
            body["reasoning"] = {"effort": self.reasoning_effort}
            body["include"] = ["reasoning.encrypted_content"]
        if self.temperature is not None:
            body["temperature"] = self.temperature
        return body

    def complete(self, messages, tools):
        """Returns (message, usage, attempts). Raises ProviderError after retries."""
        responses = self.provider == "openai"
        body = json.dumps(self.responses_payload(messages, tools) if responses
                          else self.payload(messages, tools)).encode()
        attempts = []
        for attempt in range(self.retries + 1):
            if self.sent >= self.request_cap:
                raise ProviderError("request_cap")
            self.sent += 1
            paced = _pace(self.min_interval_s)
            t0 = time.monotonic()
            req = urllib.request.Request(self.base + ("/responses" if responses else "/chat/completions"), data=body, method="POST",
                                         headers={"Authorization": "Bearer " + self._key,
                                                  "Content-Type": "application/json"})
            rec = {"attempt": attempt, "status": None, "elapsed_s": None, "wait_s": paced, "paced_s": paced}
            attempts.append(rec)
            try:
                with urllib.request.urlopen(req, timeout=self.timeout_s) as resp:
                    raw = resp.read()
                    rec["status"] = resp.status
                rec["elapsed_s"] = time.monotonic() - t0
                out = json.loads(raw)
                if responses:
                    rec["model"] = out.get("model")
                    rec["finish_reason"] = out.get("status")
                    if out.get("status") != "completed":
                        raise ProviderError("incomplete", str(out.get("incomplete_details")))
                    return _from_responses(out), normalize_responses_usage(out.get("usage")), attempts
                choice = (out.get("choices") or [{}])[0]
                msg = choice.get("message") or {}
                rec["finish_reason"] = choice.get("finish_reason")
                rec["model"] = out.get("model")
                return msg, normalize_usage(out.get("usage")), attempts
            except urllib.error.HTTPError as exc:
                rec["status"] = exc.code
                rec["elapsed_s"] = time.monotonic() - t0
                detail = exc.read()[:300].decode("utf-8", "replace")
                rec["error"] = detail[:200]
                if exc.code in (429, 500, 502, 503, 504) and attempt < self.retries:
                    wait = _retry_after(exc, attempt)
                    rec["wait_s"] += wait
                    time.sleep(wait)
                    continue
                raise ProviderError("rate_limit" if exc.code == 429 else f"http_{exc.code}", detail) from None
            except (TimeoutError, socket.timeout, urllib.error.URLError, ConnectionError) as exc:
                rec["elapsed_s"] = time.monotonic() - t0
                rec["error"] = type(exc).__name__
                if attempt < self.retries:
                    rec["wait_s"] += 2.0 * (attempt + 1)
                    time.sleep(2.0 * (attempt + 1))
                    continue
                raise ProviderError("transport", type(exc).__name__) from None
        raise ProviderError("exhausted")


def _retry_after(exc, attempt):
    try:
        value = float(exc.headers.get("Retry-After", ""))
    except (TypeError, ValueError):
        value = 0
    return max(value, min(60.0, 2.0 * 2 ** attempt))


def normalize_usage(raw):
    if not isinstance(raw, dict):
        return None
    pd = raw.get("prompt_tokens_details") or {}
    cd = raw.get("completion_tokens_details") or {}
    return {"input": int(raw.get("prompt_tokens") or 0), "output": int(raw.get("completion_tokens") or 0),
            "cached": int(pd.get("cached_tokens") or 0), "reasoning": int(cd.get("reasoning_tokens") or 0)}


def _from_responses(out):
    output = out.get("output") or []
    text = "".join(p.get("text", "") for o in output if o.get("type") == "message"
                   for p in o.get("content", []) if p.get("type") == "output_text")
    calls = [{"id": o["call_id"], "type": "function", "function": {"name": o["name"], "arguments": o["arguments"]}}
             for o in output if o.get("type") == "function_call"]
    msg = {"content": text or None, "_raw": output}
    if calls:
        msg["tool_calls"] = calls
    return msg


def normalize_responses_usage(raw):
    if not isinstance(raw, dict):
        return None
    return {"input": int(raw.get("input_tokens") or 0), "output": int(raw.get("output_tokens") or 0),
            "cached": int((raw.get("input_tokens_details") or {}).get("cached_tokens") or 0),
            "reasoning": int((raw.get("output_tokens_details") or {}).get("reasoning_tokens") or 0)}
