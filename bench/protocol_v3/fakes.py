"""Scripted responses prove mechanics, NOT model performance or generalization."""

import copy
from .core import Reply, Clock

TOKENS = {"input_tokens": 7, "output_tokens": 3, "cached_input_tokens": 2}


class ManualClock(Clock):
    def __init__(self):
        self.value = 0

    def now(self):
        return self.value

    def sleep(self, seconds):
        self.value += seconds


class ScriptedProvider:
    offline = True

    def __init__(self, responses, clock, duration=1):
        self.responses = list(responses)
        self.clock = clock
        self.duration = duration
        self.requests = []

    def request(self, request, deadline):
        self.requests.append(copy.deepcopy(request))
        self.clock.sleep(self.duration)
        if not self.responses:
            raise AssertionError("fixture responses exhausted")
        response = self.responses.pop(0)
        if isinstance(response, Exception):
            raise response
        if isinstance(response, Reply):
            return response
        return Reply(response, dict(TOKENS), "fake-revision-001")
