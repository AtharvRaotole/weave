"""Feed a streaming model response into a Weave session.

OpenAI Python SDK chat chunks look like `chunk.choices[0].delta.content`.
Anthropic SDK events look like `event.type == "content_block_delta"` and
`event.delta.text`. Dicts with the same fields are accepted too.

`Session.push` runs a statement once it is complete, so pure and read calls
start before the model finishes the rest of the script.
"""

from __future__ import annotations


def _get(obj, name: str):
    if isinstance(obj, dict):
        return obj.get(name)
    return getattr(obj, name, None)


def iter_openai_chat_chunks(chunks):
    """Yield text deltas from OpenAI-style chat completion chunks."""
    for chunk in chunks:
        choices = _get(chunk, "choices") or []
        if not choices:
            continue
        delta = _get(choices[0], "delta")
        if delta is None:
            continue
        content = _get(delta, "content")
        if content:
            yield content


def iter_anthropic_events(events):
    """Yield text from Anthropic-style content_block_delta events."""
    for event in events:
        if _get(event, "type") != "content_block_delta":
            continue
        delta = _get(event, "delta")
        if delta is None:
            continue
        text = _get(delta, "text")
        if text:
            yield text


def feed(session, pieces) -> None:
    """Push each text piece into the session as it arrives."""
    for piece in pieces:
        if piece:
            session.push(piece)
