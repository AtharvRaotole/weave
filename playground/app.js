import init, { run, check, tools as toolList, version } from "./pkg/weave_wasm.js";
import { PRESETS } from "./presets.js";

const $ = (id) => document.getElementById(id);
const src = $("src"), hl = $("hl"), gutter = $("gutter"), codeBox = src.parentElement;
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const fmtMs = (v) => `${Math.round(v)} ms`;

let TOOLS = [];
let EFFECT = {};
let ready = false;
let current = PRESETS[0];
let lastRun = null;
let animation = 0;
let errLines = new Set();

const KEYWORDS = new Set("and as break continue elif else except False finally for if in is None not or pass raise True try with while return def class import from lambda".split(" "));
const BUILTINS = new Set("print len range str int float bool list dict set sorted min max sum abs any all enumerate zip isinstance round reversed tuple Exception".split(" "));

// ---------- editor ----------

function highlight(text) {
  const re = /(#[^\n]*)|([fFrRbB]{0,2}"""[\s\S]*?(?:"""|$)|[fFrRbB]{0,2}'''[\s\S]*?(?:'''|$)|[fFrRbB]{0,2}"(?:\\.|[^"\\\n])*"?|[fFrRbB]{0,2}'(?:\\.|[^'\\\n])*'?)|\b(\d+(?:\.\d+)?)\b|([A-Za-z_][A-Za-z0-9_]*)(?=\s*\()|([A-Za-z_][A-Za-z0-9_]*)/g;
  let out = "", last = 0, m;
  while ((m = re.exec(text))) {
    out += esc(text.slice(last, m.index));
    const [tok, com, str, num, call, word] = m;
    if (com) out += `<span class="c">${esc(com)}</span>`;
    else if (str) out += `<span class="s">${esc(str)}</span>`;
    else if (num) out += `<span class="n">${num}</span>`;
    else if (call) {
      if (text[m.index - 1] === ".") out += esc(call);
      else if (EFFECT[call]) out += `<span class="t-${EFFECT[call]}">${call}</span>`;
      else if (BUILTINS.has(call)) out += `<span class="b">${call}</span>`;
      else if (KEYWORDS.has(call)) out += `<span class="k">${call}</span>`;
      else out += `<span class="t-unknown">${call}</span>`;
    } else if (word) out += KEYWORDS.has(word) ? `<span class="k">${word}</span>` : BUILTINS.has(word) ? `<span class="b">${word}</span>` : esc(word);
    else out += esc(tok);
    last = re.lastIndex;
  }
  out += esc(text.slice(last));
  return out;
}

function render() {
  const text = src.value;
  const lines = text.split("\n");
  // Highlight line by line so error lines can be tinted.
  const html = highlight(text).split("\n");
  hl.innerHTML = html.map((l, i) => (errLines.has(i + 1) ? `<span class="errline">${l || " "}</span>` : l)).join("\n") + "\n";
  gutter.innerHTML = lines.map((_, i) => `<div class="${errLines.has(i + 1) ? "err" : ""}">${i + 1}</div>`).join("");
  src.style.height = "auto";
  src.style.width = "auto";
  src.style.height = Math.max(src.scrollHeight, codeBox.clientHeight) + "px";
  src.style.width = Math.max(src.scrollWidth, codeBox.clientWidth) + "px";
}
// Note: highlighting multi-line strings across the split above can mis-nest
// spans for triple-quoted strings; the presets don't use them and it is
// purely cosmetic.

codeBox.addEventListener("scroll", () => { gutter.scrollTop = codeBox.scrollTop; });

src.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); doRun(); return; }
  if (e.key === "Tab") {
    e.preventDefault();
    const { selectionStart: s, selectionEnd: t, value: v } = src;
    if (s === t && !e.shiftKey) insert("    ");
    else {
      const a = v.lastIndexOf("\n", s - 1) + 1;
      const block = v.slice(a, t);
      const next = e.shiftKey ? block.replace(/^ {1,4}/gm, "") : block.replace(/^/gm, "    ");
      src.setRangeText(next, a, t, "select");
      onEdit();
    }
    return;
  }
  if (e.key === "Enter" && !e.shiftKey && !e.altKey) {
    const { selectionStart: s, value: v } = src;
    const lineStart = v.lastIndexOf("\n", s - 1) + 1;
    const line = v.slice(lineStart, s);
    let indent = line.match(/^\s*/)[0];
    if (/:\s*(#.*)?$/.test(line)) indent += "    ";
    e.preventDefault();
    insert("\n" + indent);
  }
});
function insert(text) {
  src.setRangeText(text, src.selectionStart, src.selectionEnd, "end");
  onEdit();
}

let checkTimer = 0;
function onEdit() {
  render();
  clearTimeout(checkTimer);
  checkTimer = setTimeout(liveCheck, 180);
}
src.addEventListener("input", onEdit);

// ---------- presets & share ----------

function presetButtons() {
  const box = $("presets");
  box.innerHTML = "";
  for (const p of PRESETS) {
    const b = document.createElement("button");
    b.role = "tab";
    b.textContent = p.name;
    b.dataset.id = p.id;
    if (p.expect === "check") b.classList.add("bad");
    b.addEventListener("click", () => loadPreset(p, true));
    box.appendChild(b);
  }
}

function selectPreset(p) {
  current = p;
  for (const b of $("presets").children) b.setAttribute("aria-selected", String(b.dataset.id === p?.id));
  $("blurb").textContent = p ? p.blurb : "A script someone shared with you. Check the plan, then hit Run.";
  const task = $("task");
  task.hidden = !p || !p.task;
  task.innerHTML = p && p.task ? `<b>The task:</b> ${esc(p.task)}` : "";
}

function loadPreset(p, andRun) {
  selectPreset(p);
  src.value = p.code;
  history.replaceState(null, "", `#${p.id}`);
  onEdit();
  if (andRun && ready) doRun(false);
}

const b64 = {
  enc: (s) => btoa(String.fromCharCode(...new TextEncoder().encode(s))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, ""),
  dec: (s) => new TextDecoder().decode(Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0))),
};

function fromHash() {
  const h = location.hash.slice(1);
  if (h.startsWith("code=")) {
    try {
      selectPreset(null);
      src.value = b64.dec(h.slice(5));
      onEdit();
      return true;
    } catch { /* fall through */ }
  }
  const p = PRESETS.find((x) => x.id === h) || PRESETS[0];
  loadPreset(p, false);
  return true;
}

$("share").addEventListener("click", async () => {
  const url = `${location.origin}${location.pathname}#code=${b64.enc(src.value)}`;
  history.replaceState(null, "", url);
  try { await navigator.clipboard.writeText(url); toast("Link copied"); } catch { toast("The link is in your address bar"); }
});
for (const el of document.querySelectorAll("[data-copy]")) {
  el.addEventListener("click", async () => {
    const text = el.dataset.copy;
    try { await navigator.clipboard.writeText(text); toast(`Copied: ${text}`); } catch { toast(text); }
  });
}
for (const el of document.querySelectorAll("[data-go]")) {
  el.addEventListener("click", () => {
    const target = $(el.dataset.go);
    if (el.dataset.go === "timeline-card") doRun(false);
    target.scrollIntoView({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" });
  });
}
$("reset").addEventListener("click", () => { if (current) loadPreset(current, false); });

function toast(msg) {
  let t = document.querySelector(".toast");
  if (!t) { t = document.createElement("div"); t.className = "toast"; document.body.appendChild(t); }
  t.textContent = msg;
  t.classList.add("show");
  setTimeout(() => t.classList.remove("show"), 1600);
}

// ---------- check ----------

function diagHtml(d) {
  const loc = d.line ? `L${d.line}:${d.col}` : "run";
  const cls = d.class === "static" ? "" : " runtime";
  return `<button class="diag${cls}" data-line="${d.line}" data-col="${d.col}"><span class="loc">${loc}</span>${esc(d.message)}${d.hint ? `<span class="hint"><b>Hint:</b> ${esc(d.hint)}</span>` : ""}</button>`;
}

function bindDiagClicks(root) {
  root.querySelectorAll(".diag[data-line]").forEach((el) =>
    el.addEventListener("click", () => jumpTo(+el.dataset.line, +el.dataset.col)));
}

function jumpTo(line, col) {
  if (!line) return;
  const lines = src.value.split("\n");
  let pos = 0;
  for (let i = 0; i < line - 1 && i < lines.length; i++) pos += lines[i].length + 1;
  pos += Math.max(0, (col || 1) - 1);
  src.focus();
  src.setSelectionRange(pos, pos);
  const lh = parseFloat(getComputedStyle(src).lineHeight) || 21;
  codeBox.scrollTop = Math.max(0, (line - 4) * lh);
}

const OK_ICON = `<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="7.25" fill="currentColor" opacity=".14"/><path d="M4.8 8.3l2.1 2.1 4.3-4.6" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
const PLAN_LINE = /^\s*#\d+ line \d+: (.*) (pure|read|reversible|irreversible) x\S+ stage \d+(?:, .*)?$/;
const plural = (n, one, many = one + "s") => `${n} ${n === 1 ? one : many}`;

function countText(c) {
  if (c.max_count === null || c.max_count === undefined) return `${c.min_count}+ times`;
  if (c.min_count === c.max_count) return c.min_count === 1 ? "once" : `${c.min_count} times`;
  return `${c.min_count} to ${c.max_count} times`;
}

function writesText(c) {
  if (!c.writes) return "";
  if (c.writes.kind === "names") return `writes ${c.writes.names.join(", ")}`;
  return "could write anything (no footprint declared)";
}

function renderPlan(plan, diags) {
  const box = $("plan-box"), note = $("plan-note");
  if (diags && diags.length) {
    note.textContent = "Waiting on the check";
    box.innerHTML = `<p class="empty">Fix what the check found and the plan shows up here.</p>`;
    return;
  }
  if (!plan || !plan.calls.length) {
    note.textContent = "Before anything runs";
    box.innerHTML = `<p class="empty">This script doesn't call any tools, so there's nothing to plan.</p>`;
    return;
  }
  const lines = plan.text.trimEnd().split("\n").slice(1);
  const writes = plan.calls.filter((c) => c.effect === "reversible" || c.effect === "irreversible");
  const stages = Math.max(...plan.calls.map((c) => c.stage || 1));
  note.textContent = "Before anything runs";
  const chips = [
    `<span class="chip"><b>${plan.calls.length}</b> ${plan.calls.length === 1 ? "tool call in the code" : "tool calls in the code"}</span>`,
    `<span class="chip"><b>${stages}</b> ${stages === 1 ? "stage" : "stages"}</span>`,
    writes.length
      ? `<span class="chip write"><b>${writes.length}</b> ${writes.length === 1 ? "write" : "writes"}</span>`
      : `<span class="chip">no writes</span>`,
  ].join("");
  const rows = plan.calls.map((c, i) => {
    const m = PLAN_LINE.exec(lines[i] || "");
    const call = m ? m[1] : c.tool + "(...)";
    const callHtml = esc(call).replace(/&lt;(#[\d,#]+)&gt;/g, '<span class="from">&lt;$1&gt;</span>');
    const isWrite = c.effect === "reversible" || c.effect === "irreversible";
    const w = writesText(c);
    return `<li class="${isWrite ? "plan-write" : ""}" data-line="${c.line}">
      <span class="num">#${i + 1}</span>
      <span class="call" title="${esc(call)}">${callHtml}</span>
      <span class="meta"><span>${countText(c)}</span><span>stage ${c.stage}</span><span class="badge ${c.effect}">${c.effect}</span></span>
      ${w ? `<span class="sub">${esc(w)}</span>` : ""}
    </li>`;
  }).join("");
  box.innerHTML = `<div class="plan-sum">${chips}</div><ol class="plan" id="plan">${rows}</ol>` +
    `<p class="note">Stage 1 calls can start right away. Stage 2 calls need a result from stage 1, and so on. <code>&lt;#1&gt;</code> means "uses the result of call #1". When it runs, calls that touch the same key also keep your order.</p>`;
  box.querySelectorAll(".plan li").forEach((li) => li.addEventListener("click", () => jumpTo(+li.dataset.line, 1)));
}

function showCheck(diags, plan) {
  const box = $("check"), dot = $("check-dot");
  errLines = new Set(diags.filter((d) => d.line).map((d) => d.line));
  render();
  renderPlan(plan, diags);
  if (!diags.length) {
    dot.className = "dot ok";
    $("check-note").textContent = "Runs as you type";
    box.innerHTML = `<p class="okmsg">${OK_ICON}<span>Looks good. Every call matches a tool and its arguments.</span></p>`;
    return;
  }
  dot.className = "dot bad";
  $("check-note").textContent = plural(diags.length, "problem");
  box.innerHTML = diags.map(diagHtml).join("") +
    `<p class="note">If you hit Run now, the script gets turned away and no tool is called.</p>`;
  bindDiagClicks(box);
}

function liveCheck() {
  if (!ready) return;
  try {
    const out = JSON.parse(check(src.value));
    showCheck(out.diagnostics, out.plan);
  } catch (e) { crashed(e); }
}

// ---------- run ----------

function doRun(scroll = true) {
  if (!ready) return;
  const btn = $("run");
  btn.disabled = true;
  let out;
  const t0 = performance.now();
  try { out = JSON.parse(run(src.value)); } catch (e) { crashed(e); return; }
  const wall = performance.now() - t0;
  btn.disabled = false;
  lastRun = out;
  if (out.stage === "check") {
    showCheck(out.diagnostics);
    setStatus(`Rejected by the check. 0 tools called.`, "bad");
    $("stats").innerHTML = "";
    $("timeline").innerHTML = `<p class="empty">The check failed, so nothing got scheduled.</p>`;
    $("effects").innerHTML = `<p class="empty">Nothing was written.</p>`;
    $("output").innerHTML = `<p class="empty">Nothing ran. Fix what the check found and try again.</p>`;
    $("replay").hidden = true;
    explain(`<b>The check turned this script away, so no tool was called.</b> That's the point: a typo can't leave a job half done, with an email already sent and the next step broken. The problems are listed under Check. Click one to jump to its line.`, "bad");
    return;
  }
  liveCheck();
  explainRun(out);
  const n = out.calls.length;
  setStatus(out.ok ? `Done. ${plural(n, "call")}, ${fmtMs(out.total_ms)} virtual` : `Stopped on an error after ${plural(n, "call")}`, out.ok ? "ok" : "bad");
  $("status").title = `Took ${wall.toFixed(1)} ms of real time in your browser`;
  renderStats(out);
  renderTimeline(out, true);
  renderEffects(out);
  renderOutput(out);
  $("replay").hidden = n === 0;
  if (scroll && matchMedia("(max-width: 1000px)").matches) $("timeline-card").scrollIntoView({ behavior: "smooth", block: "start" });
}

function explain(html, cls) {
  const box = $("explain");
  box.hidden = !html;
  box.className = `explain ${cls || ""}`;
  box.innerHTML = html || "";
}

function explainRun(out) {
  const n = out.calls.length;
  const failed = out.calls.filter((c) => !c.ok).length;
  const writes = out.calls.filter((c) => c.effect === "reversible" || c.effect === "irreversible").length;
  if (!n) {
    explain(out.ok ? "This script didn't call any tools, so there was nothing to schedule." : `<b>The script stopped with an error before calling any tools.</b> The message is under Output.`, out.ok ? "" : "bad");
    return;
  }
  const peak = peakConcurrency(out.calls);
  const parts = [];
  if (peak > 1) {
    parts.push(`<b>Weave made ${plural(n, "tool call")}, up to ${peak} at the same time.</b> Calls that didn't need each other's results started together, so the run took ${fmtMs(out.total_ms)} instead of ${fmtMs(out.sequential_ms)} one after another.`);
  } else {
    parts.push(n === 1 ? `<b>Weave made 1 tool call.</b> With a single call there's nothing to overlap.` : `<b>Weave made ${plural(n, "tool call")}, one at a time.</b> Each one had to wait for the one before it, so there was nothing to overlap.`);
  }
  if (writes) parts.push(`${writes === 1 ? "The write" : `The ${writes} writes`} ran in the order you wrote ${writes === 1 ? "it" : "them"}, after the calls ${writes === 1 ? "it" : "they"} needed.`);
  if (failed && out.ok) parts.push(`${plural(failed, "call")} failed (the striped bar) and the script caught it, so the rest carried on.`);
  if (!out.ok) parts.push(`<b>Something failed and nothing caught it, so the script stopped.</b> ${failed ? "The striped bar is the call that failed. " : ""}The error is under Output.`);
  parts.push(`<span class="muted">Times come from the fake tools' set delays.</span>`);
  explain(parts.join(" "), out.ok ? "" : "bad");
}

function setStatus(msg, cls) {
  const s = $("status");
  s.textContent = msg;
  s.className = `status ${cls || ""}`;
}

function renderStats(out) {
  const n = out.calls.length;
  if (!n) { $("stats").innerHTML = ""; return; }
  const peak = peakConcurrency(out.calls);
  const ratio = out.total_ms > 0 ? out.sequential_ms / out.total_ms : 1;
  $("stats").innerHTML = [
    stat(fmtMs(out.total_ms), "with Weave", true),
    stat(fmtMs(out.sequential_ms), "one by one"),
    stat(`${ratio.toFixed(1)}x`, "less waiting"),
    stat(String(n), n === 1 ? "call" : "calls"),
    stat(String(peak), "at once, max"),
  ].join("");
}
const stat = (b, s, hi) => `<div class="stat${hi ? " hi" : ""}"><b>${esc(b)}</b><span>${esc(s)}</span></div>`;

function peakConcurrency(calls) {
  const ev = [];
  for (const c of calls) { ev.push([c.start_ms, 1]); ev.push([c.end_ms, -1]); }
  ev.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  let cur = 0, peak = 0;
  for (const [, d] of ev) { cur += d; peak = Math.max(peak, cur); }
  return peak;
}

function callText(c) {
  const parts = [...c.args, ...Object.entries(c.kwargs).map(([k, v]) => `${k}=${v}`)];
  return { name: c.tool, args: parts.join(", ") };
}

function niceStep(total) {
  const raw = total / 5;
  const pow = 10 ** Math.floor(Math.log10(Math.max(raw, 1)));
  for (const m of [1, 2, 2.5, 5, 10]) if (raw <= m * pow) return m * pow;
  return 10 * pow;
}

function renderTimeline(out, animate) {
  cancelAnimationFrame(animation);
  const box = $("timeline");
  const calls = out.calls;
  if (!calls.length) { box.innerHTML = `<p class="empty">No tools were called.</p>`; return; }
  const total = Math.max(out.total_ms, ...calls.map((c) => c.end_ms), 1);
  const step = niceStep(total);
  const span = Math.ceil(total / step) * step;
  const pct = (v) => (100 * v) / span;
  let axis = "";
  for (let t = 0; t <= span + 1e-9; t += step) axis += `<span style="left:${pct(t)}%">${Math.round(t)}</span>`;
  const rows = calls.map((c, i) => {
    const { name, args } = callText(c);
    const cls = c.ok ? c.effect : "fail";
    return `<div class="row${c.effect === "reversible" || c.effect === "irreversible" ? " w" : ""}" data-i="${i}" title="${esc(name)}(${esc(args)})  ${Math.round(c.start_ms)} to ${Math.round(c.end_ms)} ms">
      <div class="label"><span class="t-${c.effect}">${esc(name)}</span><span class="args">(${esc(args)})</span></div>
      <div class="track"><div class="grid"></div><div class="barx ${cls}" data-s="${c.start_ms}" data-e="${c.end_ms}" style="left:${pct(c.start_ms)}%;width:${animate ? 0 : pct(c.end_ms - c.start_ms)}%"><span class="ms">${animate ? "" : fmtMs(c.end_ms - c.start_ms)}</span></div></div>
    </div>`;
  }).join("");
  box.innerHTML = `<div class="axis">${axis}</div>${rows}<p class="note">Virtual milliseconds. Click a row to see what went in and what came back.</p>`;
  box.style.setProperty("--grid", `${pct(step)}%`);
  box.querySelectorAll(".row").forEach((row) => row.addEventListener("click", () => toggleDetail(row, calls[+row.dataset.i])));
  if (animate && !matchMedia("(prefers-reduced-motion: reduce)").matches) play(box, span, pct);
  else finishBars(box, pct);
}

function finishBars(box, pct) {
  box.querySelectorAll(".barx").forEach((b) => {
    const s = +b.dataset.s, e = +b.dataset.e;
    b.style.width = `${pct(e - s)}%`;
    b.querySelector(".ms").textContent = fmtMs(e - s);
  });
}

// Replay at real speed (1 virtual ms = 1 ms), capped so long runs stay short.
function play(box, span, pct) {
  const speed = Math.max(1, span / 1600);
  const bars = [...box.querySelectorAll(".barx")];
  const t0 = performance.now();
  const frame = (now) => {
    const t = (now - t0) * speed;
    for (const b of bars) {
      const s = +b.dataset.s, e = +b.dataset.e;
      const w = Math.max(0, Math.min(t, e) - s);
      b.style.width = `${pct(w)}%`;
      b.querySelector(".ms").textContent = t >= e ? fmtMs(e - s) : "";
    }
    if (t < span) animation = requestAnimationFrame(frame);
  };
  animation = requestAnimationFrame(frame);
}
$("replay").addEventListener("click", () => lastRun && renderTimeline(lastRun, true));

function toggleDetail(row, c) {
  const next = row.nextElementSibling;
  if (next && next.classList.contains("detail")) { next.remove(); return; }
  const d = document.createElement("div");
  d.className = "detail";
  const kw = Object.entries(c.kwargs).map(([k, v]) => `${k}=${v}`).join(", ");
  d.innerHTML = `<b>${esc(c.tool)}</b>  <span class="badge ${c.effect}">${c.effect}</span>  ${Math.round(c.start_ms)} to ${Math.round(c.end_ms)} ms\n` +
    `<b>args</b>   ${esc(c.args.join(", ") || "none")}${kw ? `\n<b>kwargs</b> ${esc(kw)}` : ""}\n` +
    (c.ok ? `<b>result</b> ${esc(c.result)}` : `<b>error</b>  ${esc(c.error)}`);
  row.after(d);
}

function renderEffects(out) {
  const writes = out.calls.filter((c) => c.effect === "reversible" || c.effect === "irreversible");
  const box = $("effects");
  if (!writes.length) {
    box.innerHTML = `<p class="empty">No writes in this one, so every call could start as soon as its inputs were ready.</p>`;
    return;
  }
  const items = writes.map((c) => {
    const { name, args } = callText(c);
    return `<li><span class="badge ${c.effect}">${c.effect}</span><span class="call">${esc(name)}(${esc(args)})</span><span class="when">${Math.round(c.start_ms)} to ${Math.round(c.end_ms)} ms</span></li>`;
  }).join("");
  box.innerHTML = `<ol class="effects">${items}</ol><p class="note">Writes go one at a time, in the order you wrote them. A later read only waits for writes that could touch what it reads. A tool that doesn\'t say what it writes (like post_slack or send_email here) counts as touching everything.</p>`;
}

function renderOutput(out) {
  const box = $("output");
  let html = "";
  if (out.stdout) html += `<pre class="stdout">${esc(out.stdout)}</pre>`;
  if (!out.ok) {
    html += out.diagnostics.map(diagHtml).join("");
  }
  const vars = Object.entries(out.bindings || {});
  if (vars.length) {
    html += `<div class="vars-title">Variables</div><table class="vars">${vars.map(([k, v]) => `<tr><td>${esc(k)}</td><td>${esc(v.length > 400 ? v.slice(0, 400) + " ..." : v)}</td></tr>`).join("")}</table>`;
  }
  box.innerHTML = html || `<p class="empty">The script didn\'t print anything.</p>`;
  bindDiagClicks(box);
}

// ---------- tools ----------

function renderTools() {
  $("tools").innerHTML = TOOLS.map((t) => {
    const params = t.params.map((p) => (p.required ? p.name : `${p.name}=...`)).join(", ");
    return `<div class="tool"><span class="badge ${t.effect}">${t.effect}</span><span class="sig">${esc(t.name)}(${esc(params)})${t.returns ? `<span class="ret"> -> ${esc(t.returns)}</span>` : ""}</span><span class="lat">${t.latency_ms} ms</span><span class="about">${esc(t.about)}</span></div>`;
  }).join("");
}

function crashed(e) {
  console.error(e);
  setStatus("Something broke inside the runtime. Reloading the page...", "bad");
  const url = `${location.pathname}#code=${b64.enc(src.value)}`;
  setTimeout(() => { location.replace(url); location.reload(); }, 1200);
}

// ---------- boot ----------

$("kbd").textContent = /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘ Enter" : "Ctrl Enter";
presetButtons();
fromHash();
$("run").addEventListener("click", () => doRun());
window.addEventListener("resize", render);

init().then(() => {
  TOOLS = JSON.parse(toolList());
  EFFECT = Object.fromEntries(TOOLS.map((t) => [t.name, t.effect]));
  renderTools();
  if (matchMedia("(min-width: 1001px)").matches) $("tools-box").open = true;
  ready = true;
  $("run").disabled = false;
  $("ver").textContent = `Runtime v${version()}, compiled to WebAssembly`;
  setStatus("Ready. Pick an example or edit the script.");
  render();
  liveCheck();
  doRun(false);
}).catch((e) => {
  console.error(e);
  setStatus("Couldn\'t load WebAssembly in this browser. Try a recent Chrome, Firefox or Safari.", "bad");
});
