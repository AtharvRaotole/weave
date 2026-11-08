# Weave — positioning, README research, and product roadmap

**The most promising direction is a runtime that makes agent workflows inspectable, reproducible, and controllable.** Parallel calls are useful, but code-based tool orchestration is already an established category. Weave should earn trust through a faster debugging loop and explicit control over effects. This is a product hypothesis to validate with users, not a guarantee of adoption.

Research date: **2026-11-08**. Technical comparisons use first-party documentation and papers. Reddit threads are small, self-selected anecdotes used to generate hypotheses, not representative market evidence. The private repository's code was not submitted to public research services.

## High-traction README references

Daily active users and GitHub traffic are different from package downloads. Public evidence did not establish 500,000 daily unique users or repository visitors for the comparison set. To meet the intent of the request with a measurable scale proxy, I checked the official npm download API for React and Vite over **2026-10-25 through 2026-10-31**:

| Project | Download events in seven days | Daily average, not unique people | What its README does well |
| --- | ---: | ---: | --- |
| [React](https://github.com/facebook/react/blob/main/README.md) | 242,315,626 | 34,616,518 | Defines the category immediately; explains gradual adoption; shows a small example; routes readers to a documentation hierarchy. [Registry data](https://api.npmjs.org/downloads/point/2026-10-25:2026-10-31/react) |
| [Vite](https://github.com/vitejs/vite/blob/main/README.md) | 248,633,833 | 35,519,119 | Consistent visual identity, brief benefits, an explicit explanation of what the tool contains, and a clear documentation path. [Registry data](https://api.npmjs.org/downloads/point/2026-10-25:2026-10-31/vite) |

Download events include automated installations and repeated installs. They are not usage/retention measurements. Raw API responses are in the audit bundle.

I also examined these established developer tools for more directly useful presentation patterns, without claiming their daily-user counts:

| Reference | Pattern worth adopting | Application to Weave |
| --- | --- | --- |
| [uv](https://github.com/astral-sh/uv/blob/main/README.md) | A precise category, prominent install instructions, evidence linked to methodology, platform guidance. | Lead with what runs and how to try it; make a short path to the first successful workflow. |
| [FastAPI](https://github.com/fastapi/fastapi/blob/master/README.md) | A complete runnable example, expected behavior, documentation and support links. | Make the first example work offline; explain both registration and the generated script. |
| [Ollama](https://github.com/ollama/ollama/blob/main/README.md) | Installation is organized around the user's platform; examples follow immediately. | Distinguish wheels from building, and Python installation from the separate MCP executable. |
| [Pydantic](https://github.com/pydantic/pydantic/blob/main/README.md) | Compact identity, current compatibility information, clear help/install routes. | State alpha maturity and package naming honestly; move reference detail out of the first screen. |

The lesson is not that a particular badge layout causes adoption. These READMEs reduce uncertainty: what is it, is it for me, how do I install it, what will happen, and where do I go next?

## What changed in Weave's README

The previous README already had useful diagrams, examples, measured results, contribution guidance, and research attribution. Its weakness was the mismatch between confident claims and actual behavior.

The rewrite:

- Leads with a specific workflow benefit and a precise category.
- Provides wheel/source paths, a Windows alternative, and the package-name conflict before users install the wrong project.
- Includes a complete offline demo, expected output, and replay check.
- Separates Python-agent integration from MCP setup and explains the different outputs.
- Corrects the claim that native calling necessarily needs a turn for every independent call.
- Replaces the headline speedup framing with labeled benchmark endpoints and caveats.
- Explains effect ordering, host permissions, cancellation, replay, and resource limitations accurately.
- Gives a local documentation serving command because repository HTML links are not a hosted docs experience.

The draft intentionally identifies current limitations. Once fixes ship, replace each limitation with its verified guarantee and regression evidence. Adding stronger marketing language before that would make the README less professional.

## Where the category already stands

| Primary source | Relevant established idea | Implication for Weave |
| --- | --- | --- |
| [LLMCompiler, Kim et al.](https://arxiv.org/abs/2312.04511) | Planning dependency graphs and executing tool calls in parallel. | Automatic parallelization alone is insufficient differentiation. Compare equal workloads and completion boundaries. |
| [Anthropic: code execution with MCP](https://www.anthropic.com/research/code-execution-with-mcp) | Running control flow in code and keeping unnecessary intermediate results out of model context. | Returning full bindings and traces by default undermines a central code-execution benefit. |
| [Anthropic: advanced tool use](https://www.anthropic.com/research/advanced-tool-use) | Tool search, programmatic calls, and usage examples complement each other. | Weave's result examples are useful; add selective discovery and typed input/output contracts rather than listing every tool twice. |
| [Cloudflare Code Mode](https://blog.cloudflare.com/code-mode/) | Agents can compose tool APIs using generated code rather than only direct calls. | The small Python-like language must justify its compatibility cost through scheduling, observability, and control. |
| [Pydantic Monty](https://github.com/pydantic/monty) | An existing Rust implementation for running AI-written Python with explicit host access. | A tiny Python runtime is not an empty market. Benchmark the current version and its current APIs; do not generalize from an older sequential adapter. |
| [Nichols et al.: speculative tool calls](https://arxiv.org/abs/2512.15834) | Speculative work and a tool-cache interface can reduce tool-related inference stalls. | Speculation should be a measured opt-in optimization with wasted work and freshness accounted for. The paper does not establish arbitrary external-read cache correctness. |

A stronger positioning hypothesis: **“Understand an agent's tool workflow, control its effects, and reproduce its failures locally.”** The existing scheduler and trace machinery give Weave a starting point, but the audit's replay and failure-handling defects must be fixed first.

## Features ranked by user value

All commands and interfaces below are **proposals**, not existing features.

### 1. A workflow debugger that produces regression tests

**User problem:** a workflow fails once, and reproducing it requires another model call, API quota, and possibly another external write.

**First version:** record a source hash, tool/schema manifest, stable invocation IDs, start/end/queue times, attempt numbers, cancellation outcomes, and a partial trace even when execution fails. Provide an offline viewer showing the dependency graph and timeline, with a trace-to-fixture export. Exact replay uses recorded values and never invokes host tools. Redaction is explicit, and a redacted trace clearly indicates when it cannot be faithfully replayed.

**Why this is the strongest bet:** it turns existing runtime machinery into a developer workflow. Community discussions repeatedly mention difficult MCP testing, ambiguous failures, timing, and duplicate writes. Those are anecdotes, but they align directly with gaps confirmed in this audit. [Testing discussion](https://www.reddit.com/r/mcp/comments/1v5gqi8/is_testing_mcp_servers_just_painful_right_now/), [inspector feedback](https://www.reddit.com/r/mcp/comments/1wrl3i3/i_built_an_mcp_clientinspector_where_you_can_see/).

**Dependencies:** W01, W03, failure trace retention, stable trace identity. A viewer without correct traces would create misleading confidence.

**Acceptance:** take an injected failure from report to passing offline regression in under two minutes, with zero live calls during replay. Validate with five external developers and compare against their current debugging time.

### 2. Explain and approve the writes a script will perform

**User problem:** approving one broad script tool can conceal several unrelated actions.

**First version:** an explain view shows symbolic dependencies and effect declarations. A host-controlled execution mode resolves permitted reads and pauses before a write, showing the tool, exact arguments, and relevant evidence. Approval is tied to the resolved operation and policy; changes invalidate it. A script cannot approve itself. Enforce per-tool/resource allowlists and call/output budgets outside the model.

This is not an atomic transaction across APIs. Some branches are only known after reads, and external state can change during approval. Versioned reads or revalidation are needed where freshness affects a write.

**Dependencies:** W01–W05, cancellation, stable invocation identity.

**Acceptance:** rejection causes zero writes; approval of one operation cannot authorize a modified one. Retry with the same idempotency key when supported, and surface uncertain completion instead of silently repeating the action.

### 3. An MCP onboarding command that diagnoses the connection

**User problem:** users abandon a tool before seeing its benefit because binaries, schemas, names, or protocol assumptions do not line up.

**First version:** a proposed `weave doctor` checks the installed version, command availability, handshake, discovery pages, naming collisions, supported result formats, and effect declarations. A proposed `weave init` generates absolute-path client configuration and a safe sample workflow. It should not invoke real tools or edit another application's configuration without an explicit opt-in.

**Dependencies:** W05, W08–W12, distinct package identity, standalone binaries, actual client compatibility tests.

**Acceptance:** a new user reaches a successful read-only workflow in under five minutes on a clean supported machine. Diagnostics identify deliberately broken configurations without invoking write tools.

### 4. Compact model output and selective tool discovery

**User problem:** a workflow performs useful filtering, yet the model still receives all tool schemas and intermediate payloads.

**First version:** offer a script-only exposure mode, compact stdout/status/trace-ID results, host-side trace storage, and explicit schema discovery. Preserve structured MCP outputs and describe actual input/output types; do not require scripts to guess text layouts. Enforce privacy and output-size policy before returning results to a model.

**Dependencies:** W07–W10 and result budgets.

**Acceptance:** with 100–1,000 exposed tools and large tool results, measure schema tokens, result tokens, success rate, and latency. A synthetic sensitive value excluded by the workflow must not reappear through trace metadata.

### 5. A benchmark users can run against their own tools

**User problem:** a repository's impressive chart does not predict performance on their workflow.

**First version:** a proposed benchmark mode compares serial calls, bounded native parallel calls, and Weave under the same final-answer contract. Provide an offline fixture mode, and make paid-model runs opt-in with a budget. Show per-task wins, losses, errors, cost, p50/p95, cancellation, and wrong-write rate.

**Dependencies:** W06, stronger trace timing, versioned fixtures. Start with deterministic synthetic tools, then use several real providers and held-out workloads.

**Acceptance:** a user can reproduce a report, see when Weave is slower, and identify the reason. Publish raw results and version metadata without embedding secrets.

## A launch demo people can understand

Use a small incident-triage fixture:

1. Read service status, deployment metadata, and recent error counts from independent tools.
2. Show those reads overlap in the execution timeline.
3. Inject one tool failure and show the exact failing invocation and completed work.
4. Repair the script and replay it locally without calling an external system.
5. Preview a proposed ticket write; approve it through the host; verify one write.
6. Export the failed run as a regression fixture.

This demo connects speed, correctness, and control to one understandable problem. Use synthetic data and display measured timings. It is an implementation target, not a description of v0.1.0's current capabilities.

## Practical engineering references worth reading closely

- **[Amazon Builders' Library: Making retries safe with idempotent APIs](https://aws.amazon.com/builders-library/making-retries-safe-with-idempotent-APIs/).** Useful for request identity, ambiguous completion, and retry contracts. Apply the ideas to host tools; a runtime alone cannot add idempotency to an arbitrary remote API.
- **[Monty's resource-limit caveats](https://pydantic.dev/docs/monty/limitations/resource_limits/).** Useful because precise boundaries are as important as a list of limits. Document what is bounded, when enforcement occurs, and what happens after an abort.
- **[MCP tool protocol](https://modelcontextprotocol.io/specification/2025-06-18/server/tools).** Build conformance fixtures for paginated discovery and structured results before adding more example servers.
- **[MCP cancellation](https://modelcontextprotocol.io/specification/2025-06-18/basic/utilities/cancellation).** Separate cancellation requested, cancellation observed, and external action completion. A returned timeout is not proof that the action stopped.

## Sequence and success criteria

| Stage | Ship | Evidence needed before advancing |
| --- | --- | --- |
| Correctness | W01–W05 and worker/exception lifecycle fixes | Reproductions become passing regressions; no claimed deadline/replay failures in the targeted stress matrix |
| Read-only preview | Distinct installation identity, reliable MCP discovery, compact output, hosted docs | Five fresh users can install and finish a workflow; record setup failures and time to first result |
| Differentiated beta | Debugger, partial traces, fixture export | Users fix real integration failures faster and return to the tool without prompting |
| Write-capable beta | Host approvals, idempotency contracts, cancellation states | Tests show rejected actions do not run and uncertain writes are not automatically duplicated |
| Public performance launch | Equivalent-endpoint benchmark plus a concise demo | Reproducible results, held-out tasks, tail latency, error rate, and transparent limitations |

Track installation-to-first-success conversion, time to first useful workflow, repeat usage, failure-to-regression time, and write/cancellation correctness. Stars and download spikes are distribution signals; they do not prove users love or keep using the product.

Defer broad language expansion, speculative defaults, a hosted multi-tenant service, and a large dashboard until these fundamentals work. Each adds operational or compatibility surface before the core value has been demonstrated.
