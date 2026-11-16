"""Offline check of the BFCL harness: an oracle model that replays BFCL's ground truth must pass in both arms,
and a model that does nothing must fail. No network; run with bfcl-eval installed:
    PYTHONPATH=python:. python bench/bfcl/test_offline.py"""

import ast
import json
import types
from argparse import Namespace

import bench.bfcl.run as R


def _calls(gt_turn):
    out = []
    for s in gt_turn:
        node = ast.parse(s, mode="eval").body
        kw = {k.arg: ast.literal_eval(k.value) for k in node.keywords}
        out.append((node.func.id, [ast.literal_eval(a) for a in node.args], kw))
    return out


def fake_client(gt, arm, entry, lazy=False, reply=False):
    """Replays ground truth: per turn one step with all calls (native: tool calls; weave: one script), then text."""
    docs = {d["name"]: list(d["parameters"].get("properties", {})) for d in entry["function"]}
    state = {"turn": 0, "phase": 0}

    def create(**kw):
        n_user = sum(1 for m in kw["messages"] if isinstance(m, dict) and m.get("role") == "user")
        if n_user - 1 != state["turn"]:
            state["turn"], state["phase"] = n_user - 1, 0
        turn = gt[state["turn"]]
        msg = types.SimpleNamespace(content="done", tool_calls=None, role="assistant")
        if state["phase"] == 0 and not lazy:
            calls = _calls(turn)
            if arm == "native":
                msg.tool_calls = [types.SimpleNamespace(id=f"c{j}", type="function", function=types.SimpleNamespace(
                    name=f, arguments=json.dumps({**dict(zip(docs[f], a)), **k}))) for j, (f, a, k) in enumerate(calls)]
            else:
                src = "\n".join(f"{f}({', '.join([repr(x) for x in a] + [f'{p}={v!r}' for p, v in k.items()])})"
                                for f, a, k in calls) + "\n" + ('reply("done")\n' if reply else "")
                msg.tool_calls = [types.SimpleNamespace(id="c0", type="function", function=types.SimpleNamespace(
                    name="run_weave", arguments=json.dumps({"source": src})))]
            msg.model_dump = lambda: {"role": "assistant", "content": None}
        state["phase"] += 1
        usage = types.SimpleNamespace(prompt_tokens=100, completion_tokens=10, prompt_tokens_details=None,
                                      completion_tokens_details=None)
        return types.SimpleNamespace(choices=[types.SimpleNamespace(message=msg)], usage=usage)

    return create


def main():
    from bfcl_eval.model_handler.api_inference import openai_completion
    from bfcl_eval.utils import load_dataset_entry, load_ground_truth_entry
    entries = {e["id"]: e for e in load_dataset_entry(R.CATEGORY)}
    gts = {g["id"]: g for g in load_ground_truth_entry(R.CATEGORY)}
    base = dict(model="gpt-4.1-2025-04-14", temperature=0.001, effort=None, cache_key=False)
    real_init = openai_completion.OpenAICompletionsHandler.__init__
    for i in (0, 4, 20, 73):
        eid = f"{R.CATEGORY}_{i}"
        for k, (arm, config) in enumerate((("native", "doc"), ("weave", "doc"), ("weave", "compact,reply,learned"),
                             ("weave", "compact,learned,trace,typecheck,gate"))):
            args = Namespace(**base, weave_config=config)
            reply = "reply" in config
            for lazy in (False, True):
                def init(self, *a, **k):
                    real_init(self, *a, **k)
                    self.client = types.SimpleNamespace(chat=types.SimpleNamespace(completions=types.SimpleNamespace(
                        create=fake_client(gts[eid]["ground_truth"], arm, entries[eid], lazy, reply))))
                openai_completion.OpenAICompletionsHandler.__init__ = init
                # a fresh backend namespace per trial: the instances are module-level, keyed by this tag
                row, _ = R.one_trial(args, arm, entries[eid], gts[eid], 100 + 10 * k + lazy)
                assert row["error"] is None, row
                assert row["passed"] == (not lazy), (eid, arm, config, lazy, row["error_type"], row.get("weave"))
                if reply and not lazy:
                    assert row["agent_calls"] == row["turns"], row  # one model call per turn
                print("ok", eid, arm, config, "lazy" if lazy else "oracle", row["passed"], row["agent_calls"], row["tool_calls"])
    openai_completion.OpenAICompletionsHandler.__init__ = real_init
    print("bfcl harness offline checks passed")


if __name__ == "__main__":
    main()
