"""Argument footprints, @weave.declare footprints, and the wall-clock limit."""

import time

import weave

store = {"u1": "Ada", "u2": "Lin"}


@weave.declare(effect="read", arity=1, reads=["user:{user_id}"])
def get_user(user_id):
    return store[user_id]


@weave.declare(effect="reversible", arity=2, writes=["user:{user_id}"])
def rename(user_id, name):
    store[user_id] = name


def main():
    engine = weave.Engine()
    engine.install([get_user, rename])
    cache = engine.speculate([("get_user", ["u1"]), ("get_user", ["u2"])])
    time.sleep(0.05)
    run = engine.run('rename("u1", "Grace")\nprint(get_user("u1"), get_user("u2"))\n', speculation=cache)
    assert run.stdout == "Grace Lin\n", run.stdout
    assert (run.spec_hits, run.spec_misses, run.spec_invalidated) == (1, 1, 1), (
        run.spec_hits,
        run.spec_misses,
        run.spec_invalidated,
    )

    slow = weave.Engine()
    slow.tool("nap", lambda: time.sleep(5), effect="read", arity=0, hard_deadline=False)
    slow.set_limits(max_wall_time_ms=200)
    started = time.perf_counter()
    try:
        slow.run("nap()\n")
    except Exception as exc:  # noqa: BLE001
        assert "time limit of 200ms exceeded" in str(exc), exc
    else:
        raise AssertionError("expected the wall-clock limit to fire")
    elapsed = time.perf_counter() - started
    assert elapsed < 1.5, elapsed
    print(f"footprints_ok wall_limit_elapsed={elapsed:.3f}s")


if __name__ == "__main__":
    main()
