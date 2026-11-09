"""OpenAI Responses wire adapter. Tests use labelled loopback HTTP only.

No SDK retries, implicit environment credentials, redirects, or proxy inheritance.
Remote requests require adapter AND runner opt-in; this stage never enables them.
"""

import json
import socket
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import Request, ProxyHandler, HTTPRedirectHandler, build_opener

from .core import (
    MAX_SCRIPTS_PER_TURN,
    ProviderFailure,
    Reply,
    Usage,
    canonical,
    digest,
)
from weave.agent_tool import RULES


class NoRedirects(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class ResponsesAdapter:
    def __init__(
        self,
        endpoint,
        *,
        live_opt_in=False,
        api_key=None,
        max_live_requests=0,
        request_timeout_s=10,
    ):
        parsed = urlsplit(endpoint)
        self.offline = (
            parsed.scheme == "http"
            and parsed.hostname == "127.0.0.1"
            and parsed.port is not None
        )
        if (
            parsed.username
            or parsed.password
            or parsed.query
            or parsed.fragment
            or parsed.path != "/v1/responses"
        ):
            raise ValueError("unsupported Responses endpoint")
        if not self.offline and (
            not live_opt_in
            or parsed.scheme != "https"
            or parsed.hostname != "api.openai.com"
            or parsed.port not in (None, 443)
            or not api_key
            or max_live_requests < 1
        ):
            raise ValueError(
                "live adapter disabled; explicit credentials, endpoint and request cap required"
            )
        self.endpoint = endpoint
        self._api_key = api_key if not self.offline else None
        self.max_live_requests = max_live_requests
        if not 0 < request_timeout_s <= 600:
            raise ValueError("request timeout must be in (0, 600] seconds")
        self.request_timeout_s = request_timeout_s
        self.sent = 0
        self.opener = build_opener(ProxyHandler({}), NoRedirects())
        self.accounting = None
        self.pending = None
        self.items = []
        self.consumed_history = 0
        self.requests = []
        self.response_ids = []
        self.script_definition = None
        self.mapping = {
            "adapter": "openai-responses-v1",
            "response_origin": "synthetic_local_http"
            if self.offline
            else "live_explicit_opt_in",
            "endpoint": endpoint,
            "hidden_sdk_retries": 0,
            "seed": "harness metadata; not an OpenAI Responses wire parameter",
            "cache": "no client cache/previous_response_id; provider caching not guaranteed disabled",
            "function_feedback": "same full underlying observations; script stdout omitted",
            "request_hashes": [],
        }

    def set_accounting(self, ledger, trial_id, attempt_index):
        self.accounting = (ledger, trial_id, attempt_index)

    def record(self, event, payload):
        if self.accounting and self.accounting[0]:
            ledger, trial_id, index = self.accounting
            ledger.append(trial_id, event, {"attempt_index": index, **payload})

    def prepare(self, fixture):
        self.script_definition = fixture.weave_tool.responses_tool()
        self.mapping["fixture_contract_hash"] = digest(list(fixture.tools.values()))

    @staticmethod
    def usage(body):
        raw = body.get("usage")
        if not isinstance(raw, dict):
            return None, None
        details = {
            k: raw[k]
            for k in ("input_tokens", "output_tokens", "total_tokens")
            if k in raw
        }
        for key, child in [
            ("input_tokens_details", "cached_tokens"),
            ("output_tokens_details", "reasoning_tokens"),
        ]:
            if isinstance(raw.get(key), dict):
                details[key] = {child: raw[key][child]} if child in raw[key] else {}
        values = {
            "input_tokens": raw.get("input_tokens"),
            "output_tokens": raw.get("output_tokens"),
            "cached_input_tokens": raw.get("input_tokens_details", {}).get(
                "cached_tokens"
            )
            if isinstance(raw.get("input_tokens_details"), dict)
            else None,
        }
        try:
            normalized = Usage.validate(values)
        except ValueError:
            normalized = None
        return normalized, details

    def wire(self, request):
        if request["target"]["api_version"] != "responses-v1":
            raise ValueError("Responses API version pin mismatch")
        if self.script_definition is None:
            raise ValueError("adapter not bound to shipped tool")
        history = request["history"]
        if len(history) < self.consumed_history:
            raise ValueError("adapter conversation reused across trials")
        if self.pending is not None:
            if len(history) != self.consumed_history + 2:
                raise ValueError("missing canonical tool/grader feedback")
            feedback = history[-1]["feedback"]
            if self.pending["calls"]:
                if request["mode"] == "weave":
                    # One output per call_id (the API requires it). Several
                    # scripts in one turn each get their own observations; an
                    # invalid shape gives every call the same repair feedback.
                    n = len(self.pending["calls"])
                    parts = feedback.get("scripts")
                    outputs = (
                        [canonical(p) for p in parts]
                        if isinstance(parts, list) and len(parts) == n
                        else [canonical(feedback)] * n
                    )
                else:
                    remaining = list(feedback.get("observations", []))
                    outputs = []
                    for call in self.pending["calls"]:
                        i = next(
                            (
                                i
                                for i, e in enumerate(remaining)
                                if e["name"] == call["name"]
                                and e["arguments"] == json.loads(call["arguments"])
                            ),
                            None,
                        )
                        if i is None:
                            raise ValueError("missing native call observation")
                        outputs.append(canonical(remaining.pop(i)))
                self.items.extend(
                    {
                        "type": "function_call_output",
                        "call_id": c["call_id"],
                        "output": out,
                    }
                    for c, out in zip(self.pending["calls"], outputs)
                )
            else:
                self.items.append({"role": "user", "content": canonical(feedback)})
            self.consumed_history = len(history)
            self.pending = None
        if not self.items:
            self.items = [
                {
                    "role": "user",
                    "content": canonical(
                        {"task": request["task"], "canonical_tools": request["tools"]}
                    ),
                }
            ]
        tools = (
            [self.script_definition]
            if request["mode"] == "weave"
            else [
                {
                    "type": "function",
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t["inputSchema"],
                    "strict": True,
                }
                for t in request["tools"]
            ]
        )
        generation = request["generation"]
        payload = {
            "model": request["target"]["model_revision"],
            "instructions": request["system"]
            + "\nShared execution reference (both modes): "
            + RULES
            + " Controlled benchmark override: function results contain full canonical backend observations in both modes, not script stdout. Submit a final answer after consuming them.",
            "input": json.loads(canonical(self.items)),
            "tools": tools,
            "parallel_tool_calls": True,
            "store": False,
            "max_output_tokens": generation["max_output_tokens"],
        }
        # A pinned generation field set to None is declared "not sent": some
        # models reject temperature (reasoning models) or reasoning effort
        # (non-reasoning models). Both modes of a trial share the same config.
        if generation["temperature"] is not None:
            payload["temperature"] = generation["temperature"]
        if generation["reasoning"] is not None:
            payload["reasoning"] = {"effort": generation["reasoning"]}
        if generation.get("top_p") is not None:
            payload["top_p"] = generation["top_p"]
        return payload

    def request(self, request, deadline):
        import time

        if not self.offline and self.sent >= self.max_live_requests:
            raise ProviderFailure(
                "budget",
                usage={"input_tokens": 0, "output_tokens": 0, "cached_input_tokens": 0},
            )
        payload = self.wire(request)
        self.requests.append(json.loads(canonical(payload)))
        self.mapping["request_hashes"].append(digest(payload))
        self.sent += 1
        self.record(
            "http_begin",
            {
                "request_hash": digest(payload),
                "endpoint": self.endpoint,
                "model_revision": payload["model"],
                "body": payload,
                "usage": None,
            },
        )
        headers = {"Content-Type": "application/json"}
        if self._api_key:
            headers["Authorization"] = "Bearer " + self._api_key
        timeout = max(0.001, min(self.request_timeout_s, deadline - time.monotonic()))
        try:
            with self.opener.open(
                Request(
                    self.endpoint,
                    data=canonical(payload).encode(),
                    headers=headers,
                    method="POST",
                ),
                timeout=timeout,
            ) as response:
                raw = response.read(8 * 1024 * 1024 + 1)
                if len(raw) > 8 * 1024 * 1024:
                    raise ProviderFailure("malformed_response")
        except HTTPError as exc:
            usage = details = None
            try:
                usage, details = self.usage(json.loads(exc.read(8 * 1024 * 1024)))
            except (ValueError, TypeError, AttributeError):
                pass
            finally:
                exc.close()
            self.record(
                "http_response",
                {"http_status": exc.code, "usage": usage, "usage_details": details},
            )
            try:
                wait = float(exc.headers.get("Retry-After", "0"))
            except ValueError:
                wait = 0
            raise ProviderFailure(
                "rate_limit"
                if exc.code == 429
                else "transient"
                if exc.code >= 500
                else "http_error",
                usage=usage,
                retry_after_s=max(0, min(wait, 60)),
            ) from None
        except (TimeoutError, socket.timeout):
            raise ProviderFailure("timeout") from None
        except URLError as exc:
            raise ProviderFailure(
                "timeout"
                if isinstance(exc.reason, (socket.timeout, TimeoutError))
                else "transport_error"
            ) from None
        try:
            body = json.loads(raw)
        except (ValueError, TypeError):
            raise ProviderFailure("malformed_response") from None
        if not isinstance(body, dict):
            raise ProviderFailure("malformed_response")
        usage, details = self.usage(body)
        self.record(
            "http_response",
            {
                "http_status": 200,
                "usage": usage,
                "usage_details": details,
                "model_revision": body.get("model"),
                "response_id": body.get("id"),
                "response_status": body.get("status"),
                "output": body.get("output"),
            },
        )
        if body.get("status") != "completed":
            raise ProviderFailure("incomplete_response", usage=usage)
        output = body.get("output")
        calls = [
            o
            for o in output or []
            if isinstance(o, dict) and o.get("type") == "function_call"
        ]
        try:
            if not isinstance(output, list) or any(
                not isinstance(o, dict) for o in output
            ):
                raise ValueError()
            if len({c["call_id"] for c in calls}) != len(calls):
                raise ValueError()
            if calls:
                if request["mode"] == "weave":
                    sources = []
                    for c in calls:
                        if c["name"] != self.script_definition["name"]:
                            raise ValueError()
                        args = json.loads(c["arguments"])
                        if set(args) != {"source"} or not isinstance(
                            args["source"], str
                        ):
                            raise ValueError()
                        sources.append(args["source"])
                    # Several well-formed scripts in one turn (parallel tool
                    # calls) run in call order, like an ordinary agent loop;
                    # more than core.MAX_SCRIPTS_PER_TURN is an invalid shape
                    # that gets the generic repair feedback (rule 6).
                    decision = {
                        "script": sources[0]
                        if len(sources) == 1
                        else sources
                        if len(sources) <= MAX_SCRIPTS_PER_TURN
                        else None
                    }
                else:
                    decision = {
                        "calls": [
                            {"name": c["name"], "arguments": json.loads(c["arguments"])}
                            for c in calls
                        ]
                    }
                    if any(
                        c["name"] not in {t["name"] for t in request["tools"]}
                        for c in calls
                    ):
                        raise ValueError()
            else:
                decision = {
                    "answer": "".join(
                        p["text"]
                        for o in output
                        if o.get("type") == "message"
                        for p in o.get("content", [])
                        if p.get("type") == "output_text"
                    )
                }
        except (ValueError, TypeError, KeyError, AttributeError):
            raise ProviderFailure("malformed_response", usage=usage) from None
        self.items.extend(output)
        self.pending = {"calls": calls}
        self.response_ids.append(body.get("id"))
        return Reply(decision, usage, body.get("model", ""), details)
