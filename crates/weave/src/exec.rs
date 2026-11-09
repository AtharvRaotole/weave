use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::{join_all, FutureExt};
use futures::future::Shared;
use tokio::runtime::Handle;
use tokio::sync::{mpsc::UnboundedReceiver, watch};

use crate::analysis::{assigned_names, block_effects, has_carried_dep, is_builtin};
use crate::ast::{
    BinOp, BoolOp, CmpOp, Conversion, Expr, ExprKind, FSeg, Handler, Keyword, Stmt, StmtKind, Target,
    UnaryOp,
};
use crate::check::{check_block, check_program};
use crate::effect::Effect;
use crate::error::{Diagnostic, WeaveError};
use crate::parse::{parse_detailed, parse_program};
use crate::stream::complete_prefix_end;
use crate::cancel::{CancelSource, CancelToken};
use crate::limits::{check_value, Limits};
use crate::speculate::{PredictedCall, Predictor, SpecCache, SpecStats};
use crate::tools::{IsolatedJob, IsolatedStart, ToolBody, ToolFn, ToolRegistry, ToolSpec};
use crate::trace::{Trace, TraceBuilder, TraceEvent};
use crate::value::{self, Value};

type ValFut = Shared<Pin<Box<dyn Future<Output = Result<Value, WeaveError>> + Send>>>;
type MapFut = Shared<Pin<Box<dyn Future<Output = Result<Arc<HashMap<String, Value>>, WeaveError>> + Send>>>;

fn share(fut: impl Future<Output = Result<Value, WeaveError>> + Send + 'static) -> ValFut {
    (Box::pin(fut) as Pin<Box<dyn Future<Output = Result<Value, WeaveError>> + Send>>).shared()
}

fn share_map(
    fut: impl Future<Output = Result<Arc<HashMap<String, Value>>, WeaveError>> + Send + 'static,
) -> MapFut {
    (Box::pin(fut)
        as Pin<Box<dyn Future<Output = Result<Arc<HashMap<String, Value>>, WeaveError>> + Send>>)
        .shared()
}

fn ready(value: Value) -> ValFut {
    share(async move { Ok(value) })
}

fn fail(err: WeaveError) -> ValFut {
    share(async move { Err(err) })
}

/// Every value scheduled so far whose failure must stop later writes.
///
/// A flat list made each tool call and loop iteration copy the whole history,
/// so a loop cost grew quadratically with its iteration count. Full groups are
/// folded into one future per level, like a binary counter, so cloning and
/// awaiting stay `O(GROUP * log n)` while failures still surface in order.
#[derive(Clone, Default)]
struct PriorSet {
    /// `levels[k]` holds futures standing for `GROUP^k` values each; higher
    /// levels are older.
    levels: Vec<Vec<ValFut>>,
}

impl PriorSet {
    const GROUP: usize = 16;

    fn push(&mut self, fut: ValFut) {
        let mut carry = fut;
        let mut level = 0;
        loop {
            if self.levels.len() == level {
                self.levels.push(Vec::new());
            }
            self.levels[level].push(carry);
            if self.levels[level].len() < Self::GROUP {
                return;
            }
            let group = std::mem::take(&mut self.levels[level]);
            carry = share(async move { await_roots(group).await.map(|()| Value::Null) });
            level += 1;
        }
    }

    /// Oldest first, so the first failure reported is the earliest one.
    fn futures(&self) -> Vec<ValFut> {
        self.levels.iter().rev().flatten().cloned().collect()
    }
}

fn join_ok(a: ValFut, b: ValFut) -> ValFut {
    share(async move {
        a.await?;
        b.await?;
        Ok(Value::Null)
    })
}

#[derive(Clone)]
enum RunMode {
    Execute,
    Replay(Arc<Replay>),
}

/// A recorded trace plus which of its events have been consumed.
///
/// Calls are matched by identity (tool, args, kwargs), taking the earliest
/// unused event with that identity. Sequence numbers depend on when a call
/// was scheduled, which differs between a batch run and a streamed run
/// (a loop that starts before later lines arrive numbers its calls later).
/// Identical calls keep their relative order in every mode: two identical
/// reads separated by a write are ordered by the write barrier, and
/// identical calls with no write between them return the same value.
struct Replay {
    events: Vec<TraceEvent>,
    used: Mutex<Vec<bool>>,
    /// Event indexes per (invocation path, tool), so matching a call does not
    /// rescan the whole trace.
    by_identity: HashMap<(Vec<usize>, String), Vec<usize>>,
}

impl Replay {
    fn new(events: Vec<TraceEvent>) -> Self {
        let mut by_identity: HashMap<(Vec<usize>, String), Vec<usize>> = HashMap::new();
        for (index, event) in events.iter().enumerate() {
            by_identity.entry((event.invocation.clone(), event.tool.clone())).or_default().push(index);
        }
        Self { used: Mutex::new(vec![false; events.len()]), events, by_identity }
    }
}

struct Launch {
    handle: Handle,
    tools: Arc<ToolRegistry>,
    mode: RunMode,
    trace: Arc<Mutex<TraceBuilder>>,
    prints: Arc<Mutex<Vec<String>>>,
    output_bytes: Arc<std::sync::atomic::AtomicUsize>,
    seq: Arc<std::sync::atomic::AtomicU64>,
    complete_tx: watch::Sender<bool>,
    complete_rx: watch::Receiver<bool>,
    cancel_rx: watch::Receiver<bool>,
    cancel_tx: watch::Sender<bool>,
    last_mutation: ValFut,
    last_observable: ValFut,
    print_barrier: ValFut,
    prior: PriorSet,
    path: Vec<usize>,
    env: HashMap<String, ValFut>,
    roots: Vec<ValFut>,
    pending_spawn: Vec<ValFut>,
    speculation: Option<SpecCache>,
    limits: Limits,
    steps: Arc<std::sync::atomic::AtomicU64>,
}

/// Count `n` steps against the run's budget.
fn charge(g: &Launch, n: usize, line: usize, col: usize) -> Result<(), WeaveError> {
    // Cancellation (including the wall-clock limit) also stops pure compute.
    ensure_running(line, col, &g.cancel_rx)?;
    charge_steps(&g.steps, g.limits.max_steps, n, line, col)
}

fn charge_steps(
    steps: &std::sync::atomic::AtomicU64,
    max: u64,
    n: usize,
    line: usize,
    col: usize,
) -> Result<(), WeaveError> {
    let used = steps.fetch_add(n as u64, std::sync::atomic::Ordering::SeqCst) + n as u64;
    if used > max {
        Err(WeaveError::runtime(
            line,
            col,
            format!("step limit of {max} exceeded; loop over less data or split the work"),
        ))
    } else {
        Ok(())
    }
}

/// Enforce the value size and nesting limits on a freshly built value.
fn checked(line: usize, col: usize, max_size: usize, value: Value) -> Result<Value, WeaveError> {
    check_value(&value, max_size).map_err(|msg| WeaveError::runtime(line, col, msg))?;
    Ok(value)
}

#[derive(Clone)]
struct BarrierSnap {
    last_mutation: ValFut,
    last_observable: ValFut,
    print_barrier: ValFut,
    prior: PriorSet,
    path: Vec<usize>,
}

impl BarrierSnap {
    fn capture(g: &Launch) -> Self {
        Self {
            last_mutation: g.last_mutation.clone(),
            last_observable: g.last_observable.clone(),
            print_barrier: g.print_barrier.clone(),
            prior: g.prior.clone(),
            path: g.path.clone(),
        }
    }

    fn install(&self, g: &mut Launch) {
        g.last_mutation = self.last_mutation.clone();
        g.last_observable = self.last_observable.clone();
        g.print_barrier = self.print_barrier.clone();
        g.prior = self.prior.clone();
        g.path = self.path.clone();
    }
}

fn track(g: &mut Launch, fut: ValFut, record_root: bool) {
    if record_root {
        g.roots.push(fut.clone());
    }
    g.pending_spawn.push(fut);
}

fn spawn_pending(arc: &Arc<Mutex<Launch>>) {
    let (handle, pending) = {
        let mut g = arc.lock().expect("launch lock");
        (g.handle.clone(), std::mem::take(&mut g.pending_spawn))
    };
    for fut in pending {
        handle.spawn(async move {
            let _ = fut.await;
        });
    }
}

pub struct Engine {
    tools: ToolRegistry,
    limits: Limits,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Self {
            tools: ToolRegistry::new(),
            limits: Limits::default(),
        }
    }

    /// Replace the resource limits for later runs and sessions.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = Limits {
            max_value_size: limits.max_value_size.min(crate::limits::HARD_MAX_VALUE_SIZE),
            ..limits
        };
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Register a host tool. The default [`ToolSpec`] is irreversible with any arity.
    pub fn register<F>(&mut self, name: &str, spec: ToolSpec, func: F)
    where
        F: Fn(Vec<Value>, BTreeMap<String, Value>) -> Result<Value, String> + Send + Sync + 'static,
    {
        self.tools.register(name, spec, Arc::new(func) as ToolFn);
    }

    /// Blocking tool that should poll `token.is_cancelled()` and return.
    /// Timeout and session cancel fire the token and stop waiting.
    pub fn register_cooperative<F>(&mut self, name: &str, spec: ToolSpec, func: F)
    where
        F: Fn(Vec<Value>, BTreeMap<String, Value>, CancelToken) -> Result<Value, String>
            + Send
            + Sync
            + 'static,
    {
        self.tools
            .register_cooperative(name, spec, Arc::new(func));
    }

    /// Async tool. The future is dropped when the attempt times out or the session is cancelled.
    pub fn register_async<F, Fut>(&mut self, name: &str, spec: ToolSpec, func: F)
    where
        F: Fn(Vec<Value>, BTreeMap<String, Value>, CancelToken) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, String>> + Send + 'static,
    {
        let func = Arc::new(func);
        self.tools.register_async(
            name,
            spec,
            Arc::new(move |args, kwargs, token| Box::pin(func(args, kwargs, token))),
        );
    }

    /// Sync work the host can destroy. On timeout or session cancel Weave calls
    /// [`IsolatedJob::kill`] and stops waiting. The host must make `wait` return.
    pub fn register_isolated<F>(&mut self, name: &str, spec: ToolSpec, start: F)
    where
        F: Fn(Vec<Value>, BTreeMap<String, Value>) -> Result<IsolatedJob, String> + Send + Sync + 'static,
    {
        self.tools.register_isolated(name, spec, Arc::new(start));
    }

    pub fn check(&self, source: &str) -> Vec<Diagnostic> {
        if source.len() > self.limits.max_source_bytes {
            return vec![Diagnostic::static_err(0, 0, "aggregate source byte budget exceeded")];
        }
        match parse_detailed(source, 0) {
            Ok(parsed) => {
                let mut diags = parsed.errors;
                diags.extend(check_program(&parsed.stmts, &self.tools));
                diags
            }
            Err(err) => err.diagnostics,
        }
    }

    pub fn run(&self, source: &str) -> Result<RunResult, WeaveError> {
        self.run_with(source, None)
    }

    /// Run `source`, serving pure and read calls from `speculation` when the
    /// arguments match a prediction. See [`Engine::speculate`].
    pub fn run_with(&self, source: &str, speculation: Option<&SpecCache>) -> Result<RunResult, WeaveError> {
        let diags = self.check(source);
        if !diags.is_empty() {
            return Err(WeaveError::new(diags));
        }
        let mut session = self.open(RunMode::Execute, speculation.cloned());
        session.push(source)?;
        session.finish()
    }

    /// Start pure and read predictions immediately. Other effects are counted
    /// as rejected and are not called. Pair the cache with [`Engine::run_with`].
    pub fn speculate(&self, calls: &[PredictedCall]) -> SpecCache {
        SpecCache::start(&self.tools, calls)
    }

    /// [`Predictor::predict`] then [`Engine::speculate`].
    pub fn speculate_with(&self, predictor: &dyn Predictor, hint: &str) -> SpecCache {
        self.speculate(&predictor.predict(hint))
    }

    pub fn replay(&self, source: &str, trace: &Trace) -> Result<RunResult, WeaveError> {
        if trace.version != crate::trace::TRACE_VERSION || trace.source.trim_end() != source.trim_end() || trace.manifest != self.manifest() {
            return Err(WeaveError::runtime(0, 0, "replay diverged: exact replay requires trace v2, identical source and tool manifest"));
        }
        let diags = self.check(source);
        if !diags.is_empty() {
            return Err(WeaveError::new(diags));
        }
        let mut session = self.open(RunMode::Replay(Arc::new(Replay::new(trace.events.clone()))), None);
        session.push(source)?;
        let result = session.finish()?;
        if result.trace.events.len() != trace.events.len() {
            return Err(WeaveError::runtime(
                0,
                0,
                format!(
                    "replay diverged: program made {} call(s), trace has {}",
                    result.trace.events.len(),
                    trace.events.len()
                ),
            ));
        }
        Ok(result)
    }

    pub fn session(&self) -> Session {
        self.open(RunMode::Execute, None)
    }

    fn manifest(&self) -> BTreeMap<String, String> {
        self.tools.names().map(|name| {
            let spec = &self.tools.get(name).expect("tool").spec;
            (name.to_string(), format!("{spec:?}"))
        }).collect()
    }

    fn open(&self, mode: RunMode, speculation: Option<SpecCache>) -> Session {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(64)
            .enable_all()
            .thread_name("weave")
            .build()
            .expect("create tokio runtime");
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let (complete_tx, complete_rx) = watch::channel(false);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let trace = Arc::new(Mutex::new(TraceBuilder::bounded(self.limits.max_trace_bytes)));
        let launch = Launch {
            handle: runtime.handle().clone(),
            tools: Arc::new(self.tools.clone()),
            mode,
            trace: trace.clone(),
            prints: Arc::new(Mutex::new(Vec::new())),
            output_bytes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            complete_tx,
            complete_rx,
            cancel_rx,
            cancel_tx: cancel_tx.clone(),
            last_mutation: ready(Value::Null),
            last_observable: ready(Value::Null),
            print_barrier: ready(Value::Null),
            prior: PriorSet::default(),
            path: Vec::new(),
            env: HashMap::new(),
            roots: Vec::new(),
            pending_spawn: Vec::new(),
            speculation,
            limits: self.limits,
            steps: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        let arc = Arc::new(Mutex::new(launch));
        runtime.spawn(driver(rx, arc, result_tx));
        Session {
            runtime: Some(runtime),
            tx,
            result_rx,
            buffer: String::new(),
            source: String::new(),
            trace_budget: trace,
            max_source_bytes: self.limits.max_source_bytes,
            max_trace_bytes: self.limits.max_trace_bytes,
            manifest: self.manifest(),
            line_offset: 0,
            defined: HashSet::new(),
            tools: Arc::new(self.tools.clone()),
            failed: None,
            finished: false,
            cancel_tx,
            max_wall_time: self.limits.max_wall_time,
            clock: None,
            timed_out: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

pub struct Session {
    /// Owns the worker threads. Dropping it aborts in-flight tool calls.
    runtime: Option<tokio::runtime::Runtime>,
    tx: tokio::sync::mpsc::UnboundedSender<Msg>,
    result_rx: std::sync::mpsc::Receiver<Result<RunResult, WeaveError>>,
    buffer: String,
    source: String,
    trace_budget: Arc<Mutex<TraceBuilder>>,
    max_source_bytes: usize,
    max_trace_bytes: usize,
    manifest: BTreeMap<String, String>,
    line_offset: usize,
    defined: HashSet<String>,
    tools: Arc<ToolRegistry>,
    failed: Option<WeaveError>,
    finished: bool,
    cancel_tx: watch::Sender<bool>,
    max_wall_time: Duration,
    /// When execution started (the first statements reached the scheduler).
    clock: Option<std::time::Instant>,
    timed_out: Arc<std::sync::atomic::AtomicBool>,
}

enum Msg {
    Stmts(Vec<Stmt>),
    Finish,
}

impl Session {
    /// Start the wall-clock limit the first time code is handed to the
    /// scheduler. Time spent waiting for a model to stream code doesn't count.
    fn start_clock(&mut self) {
        if self.clock.is_some() {
            return;
        }
        self.clock = Some(std::time::Instant::now());
        let limit = self.max_wall_time;
        let cancel = self.cancel_tx.clone();
        let flag = self.timed_out.clone();
        let Some(runtime) = &self.runtime else { return };
        runtime.spawn(async move {
            tokio::time::sleep(limit).await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = cancel.send(true);
        });
    }

    fn time_limit_error(&self) -> WeaveError {
        WeaveError::runtime(
            0,
            0,
            format!(
                "time limit of {}ms exceeded; do less work per script",
                self.max_wall_time.as_millis()
            ),
        )
    }

    /// Stop work that has not started yet. In-flight host calls may still finish.
    /// Cancellation is not caught by `try`.
    /// Cloneable cancellation source for host transports. Cancellation cannot undo effects.
    pub fn cancel_source(&self) -> CancelSource { CancelSource::from_sender(self.cancel_tx.clone()) }

    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }

    pub fn push(&mut self, chunk: &str) -> Result<(), WeaveError> {
        if self.finished {
            return Err(WeaveError::runtime(0, 0, "session is already finished"));
        }
        if let Some(err) = &self.failed {
            return Err(err.clone());
        }
        if chunk.len() > self.max_source_bytes.saturating_sub(self.source.len()) {
            let err = WeaveError::budget("aggregate source byte budget exceeded");
            self.failed = Some(err.clone());
            self.cancel();
            return Err(err);
        }
        self.source.push_str(chunk);
        let mut envelope = Trace::empty();
        envelope.source = self.source.clone(); envelope.manifest = self.manifest.clone();
        let reserve = self.trace_budget.lock().expect("trace").reserve_envelope(crate::trace::json_size(&envelope)?);
        if let Err(err) = reserve { self.failed = Some(err.clone()); self.cancel(); return Err(err); }
        self.buffer.push_str(chunk);
        let end = complete_prefix_end(&self.buffer);
        if end == 0 {
            return Ok(());
        }
        let prefix = self.buffer[..end].to_string();
        let stmts = match parse_program(&prefix, self.line_offset) {
            Ok(stmts) => stmts,
            Err(err) => {
                self.failed = Some(err.clone());
                return Err(err);
            }
        };
        let mut defined = self.defined.clone();
        let diags = check_block(&stmts, &self.tools, &mut defined);
        if !diags.is_empty() {
            let err = WeaveError::new(diags);
            self.failed = Some(err.clone());
            return Err(err);
        }
        self.defined = defined;
        self.line_offset += prefix.bytes().filter(|b| *b == b'\n').count();
        self.buffer = self.buffer[end..].to_string();
        self.start_clock();
        self.tx
            .send(Msg::Stmts(stmts))
            .map_err(|_| WeaveError::runtime(0, 0, "session closed"))?;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<RunResult, WeaveError> {
        if self.finished {
            return Err(WeaveError::runtime(0, 0, "session is already finished"));
        }
        self.finished = true;
        if let Some(err) = &self.failed {
            return Err(err.clone());
        }
        if !self.buffer.is_empty() {
            let rest = std::mem::take(&mut self.buffer);
            let stmts = parse_program(&rest, self.line_offset)?;
            let mut defined = self.defined.clone();
            let diags = check_block(&stmts, &self.tools, &mut defined);
            if !diags.is_empty() {
                return Err(WeaveError::new(diags));
            }
            self.start_clock();
            self.tx
                .send(Msg::Stmts(stmts))
                .map_err(|_| WeaveError::runtime(0, 0, "session closed"))?;
        }
        self.start_clock();
        self.tx
            .send(Msg::Finish)
            .map_err(|_| WeaveError::runtime(0, 0, "session closed"))?;
        // The timer cancels the run at the limit; allow a short grace for
        // cancelled work to unwind before giving up on the driver.
        let elapsed = self.clock.map(|c| c.elapsed()).unwrap_or_default();
        let wait = self.max_wall_time.saturating_sub(elapsed) + Duration::from_millis(250);
        let timed_out = || self.timed_out.load(std::sync::atomic::Ordering::SeqCst);
        match self.result_rx.recv_timeout(wait) {
            Ok(Err(_)) if timed_out() => Err(self.time_limit_error()),
            Ok(Ok(mut result)) => {
                result.trace.source = self.source.clone();
                result.trace.manifest = self.manifest.clone();
                if crate::trace::json_size(&result.trace)? > self.max_trace_bytes {
                    return Err(WeaveError::runtime(0, 0, "aggregate trace byte budget exceeded (including source/manifest)"));
                }
                Ok(result)
            }
            Ok(Err(err)) => Err(err),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.cancel_tx.send(true);
                Err(self.time_limit_error())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(WeaveError::runtime(0, 0, "execution stopped"))
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Don't block on abandoned sync tool threads (for example after a
        // timeout or the wall-clock limit). They finish in the background and
        // their results are dropped.
        let _ = self.cancel_tx.send(true);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunResult {
    pub bindings: BTreeMap<String, Value>,
    pub prints: Vec<String>,
    pub trace: Trace,
    /// Set when the run was given a speculation cache.
    pub speculation: Option<SpecStats>,
}

impl RunResult {
    pub fn stdout(&self) -> String {
        self.prints.concat()
    }
}

async fn driver(
    mut rx: UnboundedReceiver<Msg>,
    arc: Arc<Mutex<Launch>>,
    done: std::sync::mpsc::SyncSender<Result<RunResult, WeaveError>>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            Msg::Stmts(stmts) => {
                {
                    let mut g = arc.lock().expect("launch lock");
                    let mut env = std::mem::take(&mut g.env);
                    launch_block(&mut g, &arc, &stmts, &mut env, true);
                    g.env = env;
                }
                spawn_pending(&arc);
            }
            Msg::Finish => break,
        }
    }
    {
        let g = arc.lock().expect("launch lock");
        let _ = g.complete_tx.send(true);
    }
    let roots = arc.lock().expect("launch lock").roots.clone();
    let mut first_err = None;
    for root in roots {
        if let Err(err) = root.await {
            if !err.is_unbound() {
                first_err = Some(err);
                break;
            }
        }
    }
    let env = arc.lock().expect("launch lock").env.clone();
    let mut bindings = BTreeMap::new();
    if first_err.is_none() {
        for (name, fut) in env {
            let hidden = name.starts_with(crate::parse::HIDDEN_PREFIX);
            match fut.await {
                Ok(_) if hidden => {}
                Ok(value) => {
                    bindings.insert(name, value);
                }
                Err(err) if err.is_unbound() => {}
                Err(err) => {
                    first_err = Some(err);
                    break;
                }
            }
        }
    }
    let result = if let Some(err) = first_err {
        Err(err)
    } else {
        let g = arc.lock().expect("launch lock");
        let trace = match g.trace.lock().expect("trace").finish() {
            Ok(trace) => trace,
            Err(err) => {
                let _ = done.send(Err(err));
                return;
            }
        };
        if let RunMode::Replay(expected) = &g.mode {
            if trace.events.len() != expected.events.len() {
                let _ = done.send(Err(WeaveError::runtime(
                    0,
                    0,
                    format!(
                        "replay diverged: program made {} call(s), trace has {}",
                        trace.events.len(),
                        expected.events.len()
                    ),
                )));
                return;
            }
        }
        let prints = g.prints.lock().expect("prints").clone();
        let speculation = g.speculation.as_ref().map(SpecCache::stats);
        Ok(RunResult {
            bindings,
            prints,
            trace,
            speculation,
        })
    };
    {
        let mut g = arc.lock().expect("launch lock");
        g.env.clear();
        g.roots.clear();
        g.pending_spawn.clear();
        g.last_mutation = ready(Value::Null);
        g.last_observable = ready(Value::Null);
        g.print_barrier = ready(Value::Null);
    }
    let _ = done.send(result);
}

fn launch_block(
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    stmts: &[Stmt],
    env: &mut HashMap<String, ValFut>,
    record_root: bool,
) -> Vec<ValFut> {
    let mut roots = Vec::new();
    if let Some(first) = stmts.first() {
        if let Err(err) = charge(g, stmts.len(), first.line, first.col) {
            let fut = fail(err);
            track(g, fut.clone(), record_root);
            return vec![fut];
        }
    }
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Pass => {}
            StmtKind::Expr(expr) => {
                let fut = schedule_expr(expr, g, arc, env);
                roots.push(fut.clone());
                track(g, fut, record_root);
            }
            StmtKind::Assign { targets, value } => {
                let fut = schedule_expr(value, g, arc, env);
                for target in targets {
                    bind_target(target, fut.clone(), g, arc, env);
                }
                roots.push(fut.clone());
                track(g, fut, record_root);
            }
            StmtKind::AugAssign { target, op, value } => {
                let fut = schedule_aug(target, *op, value, g, arc, env);
                roots.push(fut.clone());
                track(g, fut, record_root);
            }
            StmtKind::If { test, body, orelse } => {
                let root = schedule_if(stmt.line, stmt.col, test, body, orelse, g, arc, env);
                roots.push(root.clone());
                track(g, root, record_root);
            }
            StmtKind::For { target, iter, body } => {
                let root = schedule_for(stmt.line, stmt.col, target, iter, body, g, arc, env);
                roots.push(root.clone());
                track(g, root, record_root);
            }
            StmtKind::Try { body, handler } => {
                let root = schedule_try(stmt.line, stmt.col, body, handler, g, arc, env);
                roots.push(root.clone());
                track(g, root, record_root);
            }
        }
        if let Some(root) = roots.last() {
            g.prior.push(root.clone());
        }
    }
    roots
}

fn schedule_if(
    line: usize,
    col: usize,
    test: &Expr,
    body: &[Stmt],
    orelse: &[Stmt],
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    env: &mut HashMap<String, ValFut>,
) -> ValFut {
    let effects = block_effects(body, &g.tools).union(block_effects(orelse, &g.tools));
    let cond = schedule_expr(test, g, arc, env);
    let snap = env.clone();
    let mut written = assigned_names(body);
    written.extend(assigned_names(orelse));
    let branch_mut = g.last_mutation.clone();
    let branch_obs = g.last_observable.clone();
    let branch_print = g.print_barrier.clone();
    let branch_prior = g.prior.clone();
    let branch_path = g.path.clone();
    let body = body.to_vec();
    let orelse = orelse.to_vec();
    let arc_c = arc.clone();
    let written_task = written.clone();
    let map_fut = share_map(async move {
        let cond_v = cond.await?;
        let branch = if value::truthy(&cond_v) { body } else { orelse };
        let (roots, assigned) = {
            let mut g = arc_c.lock().expect("launch lock");
            let saved = BarrierSnap::capture(&g);
            g.last_mutation = branch_mut;
            g.last_observable = branch_obs;
            g.print_barrier = branch_print;
            g.prior = branch_prior;
            g.path = branch_path.clone();
            let mut child = snap;
            let roots = launch_block(&mut g, &arc_c, &branch, &mut child, false);
            let assigned = capture_assigned(&written_task, &child);
            saved.install(&mut g);
            (roots, assigned)
        };
        spawn_pending(&arc_c);
        await_roots(roots).await?;
        collect_assigned(assigned).await
    });
    install_placeholders(line, col, &written, &map_fut, env);
    gate_ctrl(g, effects, &map_fut);
    map_driver(map_fut)
}

fn schedule_try(
    line: usize,
    col: usize,
    body: &[Stmt],
    handler: &Handler,
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    env: &mut HashMap<String, ValFut>,
) -> ValFut {
    let effects = block_effects(body, &g.tools).union(block_effects(&handler.body, &g.tools));
    let snap = env.clone();
    let mut written = assigned_names(body);
    written.extend(assigned_names(&handler.body));
    let branch_mut = g.last_mutation.clone();
    let branch_obs = g.last_observable.clone();
    let branch_print = g.print_barrier.clone();
    let branch_prior = g.prior.clone();
    let branch_path = g.path.clone();
    let body = body.to_vec();
    let handler = handler.clone();
    let arc_c = arc.clone();
    let written_task = written.clone();
    let map_fut = share_map(async move {
        let tried = run_caught_block(
            &arc_c,
            &body,
            snap.clone(),
            &written_task,
            branch_mut.clone(),
            branch_obs.clone(),
            branch_print.clone(),
            branch_prior.clone(),
            branch_path.clone(),
        )
        .await;
        match tried {
            Ok(map) => Ok(map),
            Err((err, mut handler_env)) if catch_error(&err, &handler) => {
                if let Some(name) = &handler.name {
                    handler_env.insert(name.clone(), ready(Value::str(err.summary())));
                }
                run_caught_block(
                    &arc_c,
                    &handler.body,
                    handler_env,
                    &written_task,
                    branch_mut,
                    branch_obs,
                    branch_print,
                    branch_prior,
                    branch_path,
                )
                .await.map_err(|(err, _)| err)
            }
            Err((err, _)) => Err(err),
        }
    });
    install_placeholders(line, col, &written, &map_fut, env);
    gate_ctrl(g, effects, &map_fut);
    map_driver(map_fut)
}

fn catch_error(err: &WeaveError, handler: &Handler) -> bool {
    if err.is_cancelled() || !err.is_catchable() {
        return false;
    }
    handler.catches_all || err.is_tool()
}

async fn run_caught_block(
    arc: &Arc<Mutex<Launch>>,
    stmts: &[Stmt],
    mut child: HashMap<String, ValFut>,
    written: &HashSet<String>,
    branch_mut: ValFut,
    branch_obs: ValFut,
    branch_print: ValFut,
    branch_prior: PriorSet,
    branch_path: Vec<usize>,
) -> Result<Arc<HashMap<String, Value>>, (WeaveError, HashMap<String, ValFut>)> {
    let barriers = BarrierSnap {
        last_mutation: branch_mut, last_observable: branch_obs,
        print_barrier: branch_print, prior: branch_prior, path: branch_path,
    };
    if let Err(err) = run_ordered_block(arc, stmts, &mut child, barriers).await {
        return Err((err, child));
    }
    collect_assigned(capture_assigned(written, &child)).await.map_err(|err| (err, child))
}

/// Exception regions preserve every completed statement, including statements
/// inside a failing compound block. Shared state is local to this region; no
/// global journal races with independent parallel branches.
fn run_ordered_block<'a>(
    arc: &'a Arc<Mutex<Launch>>,
    stmts: &'a [Stmt],
    child: &'a mut HashMap<String, ValFut>,
    barriers: BarrierSnap,
) -> futures::future::BoxFuture<'a, Result<(), WeaveError>> {
    Box::pin(async move {
        for stmt in stmts {
            if matches!(stmt.kind, StmtKind::If { .. } | StmtKind::For { .. } | StmtKind::Try { .. }) {
                let g = arc.lock().expect("launch lock");
                charge(&g, 1, stmt.line, stmt.col)?;
            }
            match &stmt.kind {
                StmtKind::If { test, body, orelse } => {
                    let value = ordered_expr(arc, test, child, &barriers).await?;
                    let chosen = if value::truthy(&value) { body } else { orelse };
                    run_ordered_block(arc, chosen, child, barriers.clone()).await?;
                }
                StmtKind::For { target, iter, body } => {
                    let value = ordered_expr(arc, iter, child, &barriers).await?;
                    let items = iter_items(stmt.line, stmt.col, value)?;
                    for (iteration, item) in items.into_iter().enumerate() {
                        {
                            let g = arc.lock().expect("launch lock");
                            charge(&g, 1, stmt.line, stmt.col)?;
                        }
                        bind_ready(target, item, child);
                        let mut nested = barriers.clone();
                        nested.path.extend([stmt.line, stmt.col, iteration]);
                        run_ordered_block(arc, body, child, nested).await?;
                    }
                }
                StmtKind::Try { body, handler } => {
                    if let Err(err) = run_ordered_block(arc, body, child, barriers.clone()).await {
                        if !catch_error(&err, handler) { return Err(err); }
                        if let Some(name) = &handler.name {
                            child.insert(name.clone(), ready(Value::str(err.summary())));
                        }
                        run_ordered_block(arc, &handler.body, child, barriers.clone()).await?;
                    }
                }
                _ => {
                    let before = child.clone();
                    let (roots, assigned) = {
                        let mut guard = arc.lock().expect("launch lock");
                        let saved = BarrierSnap::capture(&guard);
                        barriers.install(&mut guard);
                        let roots = launch_block(&mut guard, arc, std::slice::from_ref(stmt), child, false);
                        let assigned = capture_assigned(&assigned_names(std::slice::from_ref(stmt)), child);
                        saved.install(&mut guard);
                        (roots, assigned)
                    };
                    spawn_pending(arc);
                    if let Err(err) = await_roots(roots).await {
                        *child = before;
                        return Err(err);
                    }
                    match collect_assigned(assigned).await {
                        Ok(values) => for (name, value) in values.iter() {
                            child.insert(name.clone(), ready(value.clone()));
                        },
                        Err(err) => { *child = before; return Err(err); }
                    }
                }
            }
        }
        Ok(())
    })
}

fn ordered_expr(
    arc: &Arc<Mutex<Launch>>, expr: &Expr, child: &HashMap<String, ValFut>, barriers: &BarrierSnap,
) -> ValFut {
    let fut = {
        let mut g = arc.lock().expect("launch lock");
        let saved = BarrierSnap::capture(&g);
        barriers.install(&mut g);
        let fut = schedule_expr(expr, &mut g, arc, child);
        saved.install(&mut g);
        fut
    };
    spawn_pending(arc);
    fut
}


fn schedule_for(
    line: usize,
    col: usize,
    target: &Target,
    iter: &Expr,
    body: &[Stmt],
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    env: &mut HashMap<String, ValFut>,
) -> ValFut {
    let effects = block_effects(body, &g.tools);
    let carried = has_carried_dep(body);
    let parallel = !carried && effects.max <= Effect::Read;
    let iter_fut = schedule_expr(iter, g, arc, env);
    let snap = env.clone();
    let written = assigned_names(std::slice::from_ref(&Stmt {
        line,
        col,
        kind: StmtKind::For {
            target: target.clone(),
            iter: iter.clone(),
            body: body.to_vec(),
        },
    }));
    let branch_mut = g.last_mutation.clone();
    let branch_obs = g.last_observable.clone();
    let branch_print = g.print_barrier.clone();
    let branch_prior = g.prior.clone();
    let branch_path = g.path.clone();
    let body = body.to_vec();
    let target = target.clone();
    let arc_c = arc.clone();
    let written_task = written.clone();
    let steps = g.steps.clone();
    let max_steps = g.limits.max_steps;
    let map_fut = share_map(async move {
        // Once the budget is spent, don't even build the iterable.
        charge_steps(&steps, max_steps, 0, line, col)?;
        let iter_v = iter_fut.await?;
        let items = iter_items(line, col, iter_v)?;
        if parallel {
            run_parallel_for(line, col, &target, &body, &items, snap, &written_task, branch_mut, branch_obs, branch_print, branch_prior, branch_path, &arc_c).await
        } else {
            run_sequential_for(line, col, &target, &body, &items, snap, &written_task, branch_mut, branch_obs, branch_print, branch_prior, branch_path, &arc_c).await
        }
    });
    install_placeholders(line, col, &written, &map_fut, env);
    gate_ctrl(g, effects, &map_fut);
    map_driver(map_fut)
}

fn gate_ctrl(g: &mut Launch, effects: crate::analysis::Effects, map_fut: &MapFut) {
    let driver = map_driver(map_fut.clone());
    if effects.has_print {
        g.print_barrier = join_ok(g.print_barrier.clone(), driver.clone());
    }
    if effects.max >= Effect::Read {
        g.last_observable = join_ok(g.last_observable.clone(), driver.clone());
    }
    if effects.max >= Effect::Reversible {
        g.last_mutation = join_ok(g.last_mutation.clone(), driver.clone());
    }
}

fn map_driver(map_fut: MapFut) -> ValFut {
    share(async move {
        map_fut.await?;
        Ok(Value::Null)
    })
}

fn install_placeholders(line: usize, col: usize, written: &HashSet<String>, map_fut: &MapFut, env: &mut HashMap<String, ValFut>) {
    for name in written {
        let map_fut = map_fut.clone();
        let name_c = name.clone();
        let fut = share(async move {
            let map = map_fut.await?;
            match map.get(&name_c) {
                Some(value) => Ok(value.clone()),
                None => Err(WeaveError::one(Diagnostic::unbound(line, col, &name_c))),
            }
        });
        env.insert(name.clone(), fut);
    }
}

fn capture_assigned(written: &HashSet<String>, env: &HashMap<String, ValFut>) -> Vec<(String, ValFut)> {
    written
        .iter()
        .filter_map(|name| env.get(name).map(|fut| (name.clone(), fut.clone())))
        .collect()
}

async fn collect_assigned(assigned: Vec<(String, ValFut)>) -> Result<Arc<HashMap<String, Value>>, WeaveError> {
    let mut map = HashMap::new();
    for (name, fut) in assigned {
        match fut.await {
            Ok(value) => {
                map.insert(name, value);
            }
            Err(err) if err.is_unbound() => {}
            Err(err) => return Err(err),
        }
    }
    Ok(Arc::new(map))
}

async fn await_roots(roots: Vec<ValFut>) -> Result<(), WeaveError> {
    for result in join_all(roots).await {
        result?;
    }
    Ok(())
}

async fn run_parallel_for(
    line: usize,
    col: usize,
    target: &Target,
    body: &[Stmt],
    items: &[Value],
    snap: HashMap<String, ValFut>,
    written: &HashSet<String>,
    branch_mut: ValFut,
    branch_obs: ValFut,
    branch_print: ValFut,
    branch_prior: PriorSet,
    branch_path: Vec<usize>,
    arc: &Arc<Mutex<Launch>>,
) -> Result<Arc<HashMap<String, Value>>, WeaveError> {
    let planned = {
        let mut g = arc.lock().expect("launch lock");
        charge(&g, items.len(), line, col)?;
        let saved = BarrierSnap::capture(&g);
        g.last_mutation = branch_mut;
        g.last_observable = branch_obs;
        g.print_barrier = branch_print;
            g.prior = branch_prior;
            g.path = branch_path.clone();
        let mut planned = Vec::new();
        // Inside an iteration, a written name starts unbound. The analysis
        // guarantees a parallel body never reads it before assigning it, so
        // this only matters for names an iteration may not assign at all
        // (e.g. under an `if`): they stay unbound and the merge below takes
        // the value from the last iteration that did assign them, as Python
        // does, instead of the pre-loop value.
        let mut base = snap.clone();
        for name in written {
            if base.contains_key(name) {
                base.insert(name.clone(), fail(WeaveError::one(Diagnostic::unbound(line, col, name))));
            }
        }
        for (iteration, item) in items.iter().enumerate() {
            g.path = branch_path.clone();
            g.path.extend([line, col, iteration]);
            let mut child = base.clone();
            bind_ready(target, item.clone(), &mut child);
            let roots = launch_block(&mut g, arc, body, &mut child, false);
            let assigned = capture_assigned(written, &child);
            planned.push((roots, assigned));
        }
        saved.install(&mut g);
        planned
    };
    spawn_pending(arc);
    let mut all_roots = Vec::new();
    for (roots, _) in &planned {
        all_roots.extend(roots.iter().cloned());
    }
    await_roots(all_roots).await?;
    let mut merged: HashMap<String, Value> = HashMap::new();
    for (_roots, assigned) in planned.into_iter().rev() {
        let map = collect_assigned(assigned).await?;
        for (k, v) in map.iter() {
            merged.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    fill_from_snapshot(written, &snap, &mut merged).await?;
    let _ = (line, col);
    Ok(Arc::new(merged))
}

async fn run_sequential_for(
    line: usize,
    col: usize,
    target: &Target,
    body: &[Stmt],
    items: &[Value],
    snap: HashMap<String, ValFut>,
    written: &HashSet<String>,
    branch_mut: ValFut,
    branch_obs: ValFut,
    branch_print: ValFut,
    branch_prior: PriorSet,
    branch_path: Vec<usize>,
    arc: &Arc<Mutex<Launch>>,
) -> Result<Arc<HashMap<String, Value>>, WeaveError> {
    let mut local_mut = branch_mut;
    let mut local_obs = branch_obs;
    let mut local_print = branch_print;
    let mut local_prior = branch_prior;
    let mut carried: HashMap<String, Value> = HashMap::new();
    {
        let g = arc.lock().expect("launch lock");
        charge(&g, items.len(), line, col)?;
    }
    for (iteration, item) in items.iter().enumerate() {
        let (roots, assigned) = {
            let mut g = arc.lock().expect("launch lock");
            let saved = BarrierSnap::capture(&g);
            g.last_mutation = local_mut.clone();
            g.last_observable = local_obs.clone();
            g.print_barrier = local_print.clone();
            g.prior = local_prior.clone();
            g.path = branch_path.clone();
            g.path.extend([line, col, iteration]);
            let mut child = snap.clone();
            for (name, value) in &carried {
                child.insert(name.clone(), ready(value.clone()));
            }
            bind_ready(target, item.clone(), &mut child);
            let roots = launch_block(&mut g, arc, body, &mut child, false);
            let assigned = capture_assigned(written, &child);
            local_mut = g.last_mutation.clone();
            local_obs = g.last_observable.clone();
            local_print = g.print_barrier.clone();
            local_prior = g.prior.clone();
            saved.install(&mut g);
            (roots, assigned)
        };
        spawn_pending(arc);
        await_roots(roots).await?;
        let map = collect_assigned(assigned).await?;
        for (k, v) in map.iter() {
            carried.insert(k.clone(), v.clone());
        }
    }
    fill_from_snapshot(written, &snap, &mut carried).await?;
    let _ = (line, col);
    Ok(Arc::new(carried))
}

async fn fill_from_snapshot(
    written: &HashSet<String>,
    snap: &HashMap<String, ValFut>,
    into: &mut HashMap<String, Value>,
) -> Result<(), WeaveError> {
    for name in written {
        if into.contains_key(name) {
            continue;
        }
        if let Some(fut) = snap.get(name).cloned() {
            match fut.await {
                Ok(value) => {
                    into.insert(name.clone(), value);
                }
                Err(err) if err.is_unbound() => {}
                Err(err) => return Err(err),
            }
        }
    }
    Ok(())
}

fn iter_items(line: usize, col: usize, value: Value) -> Result<Vec<Value>, WeaveError> {
    match value {
        Value::List(xs) => Ok(xs),
        Value::Str(s) => Ok(s.chars().map(|ch| Value::Str(ch.to_string())).collect()),
        other => Err(WeaveError::runtime(
            line,
            col,
            format!("for expects a list or str, got {}", value::type_name(&other)),
        )),
    }
}

fn bind_ready(target: &Target, value: Value, env: &mut HashMap<String, ValFut>) {
    match target {
        Target::Name { name, .. } => {
            env.insert(name.clone(), ready(value));
        }
        Target::Unpack { targets, line, col } => match value {
            Value::List(xs) if xs.len() == targets.len() => {
                for (target, item) in targets.iter().zip(xs) {
                    bind_ready(target, item, env);
                }
            }
            _ => {
                let err = fail(WeaveError::runtime(
                    *line,
                    *col,
                    format!("cannot unpack {} into {} name(s)", value::py_repr(&value), targets.len()),
                ));
                for target in targets {
                    if let Target::Name { name, .. } = target {
                        env.insert(name.clone(), err.clone());
                    }
                }
            }
        },
        Target::Subscript { .. } => {}
    }
}

fn bind_target(target: &Target, value: ValFut, g: &mut Launch, arc: &Arc<Mutex<Launch>>, env: &mut HashMap<String, ValFut>) {
    match target {
        Target::Name { name, .. } => {
            env.insert(name.clone(), value);
        }
        Target::Unpack { line, col, targets } => {
            let line = *line;
            let col = *col;
            let n = targets.len();
            for (i, target) in targets.iter().enumerate() {
                let value = value.clone();
                let piece = share(async move {
                    let got = value.await?;
                    let items = match &got {
                        Value::List(xs) => xs,
                        other => {
                            return Err(WeaveError::runtime(
                                line,
                                col,
                                format!("cannot unpack {}", value::type_name(other)),
                            ));
                        }
                    };
                    if items.len() != n {
                        return Err(WeaveError::runtime(
                            line,
                            col,
                            format!("cannot unpack {} items into {n} name(s)", items.len()),
                        ));
                    }
                    Ok(items[i].clone())
                });
                bind_target(target, piece, g, arc, env);
            }
        }
        Target::Subscript { line, col, base, index } => {
            let line = *line;
            let col = *col;
            let ExprKind::Name(name) = &base.kind else {
                return;
            };
            let idx = schedule_expr(index, g, arc, env);
            let old = env.get(name).cloned().unwrap_or_else(|| {
                fail(WeaveError::one(Diagnostic::unbound(line, col, name)))
            });
            let name_c = name.clone();
            let max = g.limits.max_value_size;
            let updated = share(async move {
                let container = old.await?;
                let index = idx.await?;
                let value = value.await?;
                let value = value::set_item(&container, &index, value)
                    .map_err(|msg| WeaveError::runtime(line, col, msg))?;
                checked(line, col, max, value)
            });
            env.insert(name_c, updated);
        }
    }
}

fn schedule_aug(
    target: &Target,
    op: BinOp,
    value: &Expr,
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    env: &mut HashMap<String, ValFut>,
) -> ValFut {
    match target {
        Target::Name { line, col, name } => {
            let old = env.get(name).cloned().unwrap_or_else(|| {
                fail(WeaveError::one(Diagnostic::unbound(*line, *col, name)))
            });
            let rhs = schedule_expr(value, g, arc, env);
            let line = *line;
            let col = *col;
            let max = g.limits.max_value_size;
            let updated = share(async move {
                let left = old.await?;
                let right = rhs.await?;
                let value = apply_bin(op, &left, &right).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                checked(line, col, max, value)
            });
            env.insert(name.clone(), updated.clone());
            updated
        }
        Target::Subscript { line, col, base, index } => {
            let Some(name) = base_name(base) else {
                return fail(WeaveError::runtime(*line, *col, "augmented assignment only supports a name or name[index]"));
            };
            let old = env.get(&name).cloned().unwrap_or_else(|| {
                fail(WeaveError::one(Diagnostic::unbound(*line, *col, &name)))
            });
            let idx = schedule_expr(index, g, arc, env);
            let rhs = schedule_expr(value, g, arc, env);
            let line = *line;
            let col = *col;
            let max = g.limits.max_value_size;
            let updated = share(async move {
                let container = old.await?;
                let index = idx.await?;
                let current = value::get_item(&container, &index).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                let right = rhs.await?;
                let new_value = apply_bin(op, &current, &right).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                let value = value::set_item(&container, &index, new_value).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                checked(line, col, max, value)
            });
            env.insert(name, updated.clone());
            updated
        }
        Target::Unpack { line, col, .. } => fail(WeaveError::runtime(
            *line,
            *col,
            "augmented assignment does not support unpacking",
        )),
    }
}

fn base_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.clone()),
        _ => None,
    }
}

fn schedule_expr(expr: &Expr, g: &mut Launch, arc: &Arc<Mutex<Launch>>, env: &HashMap<String, ValFut>) -> ValFut {
    let inner = schedule_expr_inner(expr, g, arc, env);
    let (line, col, max) = (expr.line, expr.col, g.limits.max_value_size);
    let fut = share(async move { checked(line, col, max, inner.await?) });
    g.prior.push(fut.clone());
    fut
}

fn schedule_expr_inner(expr: &Expr, g: &mut Launch, arc: &Arc<Mutex<Launch>>, env: &HashMap<String, ValFut>) -> ValFut {
    let line = expr.line;
    let col = expr.col;
    match &expr.kind {
        ExprKind::None => ready(Value::Null),
        ExprKind::Bool(v) => ready(Value::Bool(*v)),
        ExprKind::Int(v) => ready(Value::Int(*v)),
        ExprKind::Float(v) => ready(Value::Float(*v)),
        ExprKind::Str(v) => match checked(line, col, g.limits.max_value_size, Value::Str(v.clone())) {
            Ok(value) => ready(value),
            Err(err) => fail(err),
        },
        ExprKind::Name(name) => env.get(name).cloned().unwrap_or_else(|| {
            fail(WeaveError::one(Diagnostic::unbound(line, col, name)))
        }),
        ExprKind::List(xs) | ExprKind::Tuple(xs) => {
            let parts: Vec<ValFut> = xs.iter().map(|e| schedule_expr(e, g, arc, env)).collect();
            let max = g.limits.max_value_size;
            share(async move {
                let mut out = Vec::with_capacity(parts.len());
                for part in join_all(parts).await {
                    out.push(part?);
                }
                checked(line, col, max, Value::List(out))
            })
        }
        ExprKind::Dict(items) => {
            let parts: Vec<(ValFut, ValFut)> = items
                .iter()
                .map(|(k, v)| (schedule_expr(k, g, arc, env), schedule_expr(v, g, arc, env)))
                .collect();
            let max = g.limits.max_value_size;
            share(async move {
                let mut out = Vec::with_capacity(parts.len());
                for (k, v) in parts {
                    let key = k.await?;
                    let value = v.await?;
                    value::dict_insert(&mut out, key, value).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                }
                checked(line, col, max, Value::Dict(out))
            })
        }
        ExprKind::BinOp { left, op, right } => {
            let op = *op;
            let left = schedule_expr(left, g, arc, env);
            let right = schedule_expr(right, g, arc, env);
            let max = g.limits.max_value_size;
            share(async move {
                let (l, r) = futures::future::join(left, right).await;
                let value = apply_bin(op, &l?, &r?).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                checked(line, col, max, value)
            })
        }
        ExprKind::Unary { op, operand } => {
            let op = *op;
            let operand = schedule_expr(operand, g, arc, env);
            share(async move {
                let value = operand.await?;
                apply_unary(op, &value).map_err(|msg| WeaveError::runtime(line, col, msg))
            })
        }
        ExprKind::BoolOp { op, values } => schedule_bool(line, col, *op, values, g, arc, env),
        ExprKind::Compare { left, ops, comparators } => {
            schedule_compare(line, col, left, ops, comparators, g, arc, env)
        }
        ExprKind::IfExp { test, body, orelse } => {
            let test_fut = schedule_expr(test, g, arc, env);
            let env_snap = env.clone();
            let body = body.as_ref().clone();
            let orelse = orelse.as_ref().clone();
            let arc_c = arc.clone();
            let barriers = BarrierSnap::capture(g);
            share(async move {
                let cond = test_fut.await?;
                let chosen = if value::truthy(&cond) { body } else { orelse };
                let fut = {
                    let mut g = arc_c.lock().expect("launch lock");
                    let saved = BarrierSnap::capture(&g);
                    barriers.install(&mut g);
                    let fut = schedule_expr(&chosen, &mut g, &arc_c, &env_snap);
                    saved.install(&mut g);
                    fut
                };
                fut.await
            })
        }
        ExprKind::Attribute { .. } => fail(WeaveError::runtime(
            line,
            col,
            "attribute access is only supported as a method call",
        )),
        ExprKind::Subscript { value, index } => match &index.kind {
            ExprKind::Slice { lower, upper, step } => {
                let base = schedule_expr(value, g, arc, env);
                let lower = lower.as_ref().map(|e| schedule_expr(e, g, arc, env));
                let upper = upper.as_ref().map(|e| schedule_expr(e, g, arc, env));
                let step = step.as_ref().map(|e| schedule_expr(e, g, arc, env));
                share(async move {
                    let base = base.await?;
                    let lower = match lower {
                        Some(f) => Some(f.await?),
                        None => None,
                    };
                    let upper = match upper {
                        Some(f) => Some(f.await?),
                        None => None,
                    };
                    let step = match step {
                        Some(f) => Some(f.await?),
                        None => None,
                    };
                    value::get_slice(&base, lower.as_ref(), upper.as_ref(), step.as_ref())
                        .map_err(|msg| WeaveError::runtime(line, col, msg))
                })
            }
            _ => {
                let base = schedule_expr(value, g, arc, env);
                let index = schedule_expr(index, g, arc, env);
                share(async move {
                    let base = base.await?;
                    let index = index.await?;
                    value::get_item(&base, &index).map_err(|msg| WeaveError::runtime(line, col, msg))
                })
            }
        },
        ExprKind::Slice { .. } => fail(WeaveError::runtime(line, col, "slice is only supported as an index")),
        ExprKind::FString(segs) => schedule_fstring(line, col, segs, g, arc, env),
        ExprKind::Call { func, args, keywords } => schedule_call(line, col, func, args, keywords, g, arc, env),
    }
}

fn schedule_bool(
    line: usize,
    col: usize,
    op: BoolOp,
    values: &[Expr],
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    env: &HashMap<String, ValFut>,
) -> ValFut {
    if values.is_empty() {
        return ready(Value::Bool(matches!(op, BoolOp::And)));
    }
    let first = schedule_expr(&values[0], g, arc, env);
    let rest = values[1..].to_vec();
    let env_snap = env.clone();
    let arc_c = arc.clone();
    let barriers = BarrierSnap::capture(g);
    share(async move {
        let mut last = first.await?;
        for expr in rest {
            let stop = match op {
                BoolOp::And => !value::truthy(&last),
                BoolOp::Or => value::truthy(&last),
            };
            if stop {
                return Ok(last);
            }
            let fut = {
                let mut g = arc_c.lock().expect("launch lock");
                let saved = BarrierSnap::capture(&g);
                barriers.clone().install(&mut g);
                let fut = schedule_expr(&expr, &mut g, &arc_c, &env_snap);
                saved.install(&mut g);
                fut
            };
            last = fut.await?;
        }
        let _ = (line, col);
        Ok(last)
    })
}

fn schedule_compare(
    line: usize,
    col: usize,
    left: &Expr,
    ops: &[CmpOp],
    comparators: &[Expr],
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    env: &HashMap<String, ValFut>,
) -> ValFut {
    let left_fut = schedule_expr(left, g, arc, env);
    let ops = ops.to_vec();
    let comparators = comparators.to_vec();
    let env_snap = env.clone();
    let arc_c = arc.clone();
    let barriers = BarrierSnap::capture(g);
    share(async move {
        let mut prev = left_fut.await?;
        for (op, expr) in ops.into_iter().zip(comparators) {
            let fut = {
                let mut g = arc_c.lock().expect("launch lock");
                let saved = BarrierSnap::capture(&g);
                barriers.clone().install(&mut g);
                let fut = schedule_expr(&expr, &mut g, &arc_c, &env_snap);
                saved.install(&mut g);
                fut
            };
            let next = fut.await?;
            let ok = value::compare(to_cmp(op), &prev, &next).map_err(|msg| WeaveError::runtime(line, col, msg))?;
            if !value::truthy(&ok) {
                return Ok(Value::Bool(false));
            }
            prev = next;
        }
        Ok(Value::Bool(true))
    })
}

fn schedule_fstring(
    line: usize,
    col: usize,
    segs: &[FSeg],
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    env: &HashMap<String, ValFut>,
) -> ValFut {
    let mut parts = Vec::new();
    for seg in segs {
        match seg {
            FSeg::Lit(text) => parts.push(FPart::Lit(text.clone())),
            FSeg::Interp { expr, conversion, debug } => parts.push(FPart::Interp {
                fut: schedule_expr(expr, g, arc, env),
                conversion: *conversion,
                debug: debug.clone(),
            }),
        }
    }
    let max = g.limits.max_value_size;
    share(async move {
        let mut out = String::new();
        for part in parts {
            match part {
                FPart::Lit(text) => out.push_str(&text),
                FPart::Interp { fut, conversion, debug } => {
                    let value = fut.await?;
                    if let Some(debug) = debug {
                        out.push_str(&debug);
                        if !debug.ends_with('=') {
                            out.push('=');
                        }
                    }
                    let rendered = match conversion {
                        Conversion::None | Conversion::Str => value::py_str(&value),
                        Conversion::Repr | Conversion::Ascii => value::py_repr(&value),
                    };
                    out.push_str(&rendered);
                }
            }
        }
        checked(line, col, max, Value::Str(out))
    })
}

enum FPart {
    Lit(String),
    Interp { fut: ValFut, conversion: Conversion, debug: Option<String> },
}

fn schedule_call(
    line: usize,
    col: usize,
    func: &Expr,
    args: &[Expr],
    keywords: &[Keyword],
    g: &mut Launch,
    arc: &Arc<Mutex<Launch>>,
    env: &HashMap<String, ValFut>,
) -> ValFut {
    let arg_futs: Vec<ValFut> = args.iter().map(|arg| schedule_expr(arg, g, arc, env)).collect();
    let kw_futs: Vec<(String, ValFut)> = keywords
        .iter()
        .filter_map(|kw| kw.name.clone().map(|name| (name, schedule_expr(&kw.value, g, arc, env))))
        .collect();
    match &func.kind {
        ExprKind::Name(name) if name == "print" => schedule_print(line, col, arg_futs, kw_futs, g),
        ExprKind::Name(name) if is_builtin(name) => {
            let name = name.clone();
            let max = g.limits.max_value_size;
            share(async move {
                let args = join_values(arg_futs).await?;
                if !kw_futs.is_empty() {
                    return Err(WeaveError::runtime(line, col, format!("builtin `{name}` does not take keyword arguments")));
                }
                let value = eval_builtin(&name, &args).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                checked(line, col, max, value)
            })
        }
        ExprKind::Name(name) => schedule_tool(line, col, name, arg_futs, kw_futs, g),
        ExprKind::Attribute { value, attr } => {
            let recv = schedule_expr(value, g, arc, env);
            let attr = attr.clone();
            let max = g.limits.max_value_size;
            share(async move {
                let recv = recv.await?;
                let mut args = join_values(arg_futs).await?;
                if !kw_futs.is_empty() {
                    let kwargs = join_kwargs(kw_futs).await?;
                    args = method_keywords(&attr, args, kwargs).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                }
                let value = call_method(&recv, &attr, &args).map_err(|msg| WeaveError::runtime(line, col, msg))?;
                checked(line, col, max, value)
            })
        }
        _ => fail(WeaveError::runtime(
            line,
            col,
            "only tool names, builtins, and methods can be called",
        )),
    }
}

fn schedule_print(
    line: usize,
    col: usize,
    arg_futs: Vec<ValFut>,
    kw_futs: Vec<(String, ValFut)>,
    g: &mut Launch,
) -> ValFut {
    let prev = g.print_barrier.clone();
    let prints = g.prints.clone();
    let output_bytes = g.output_bytes.clone();
    let max_output_bytes = g.limits.max_output_bytes;
    let cancel = g.cancel_tx.clone();
    let fut = share(async move {
        let args_fut = join_values(arg_futs);
        let kwargs_fut = join_kwargs(kw_futs);
        let (args, kwargs, _) = futures::future::join3(args_fut, kwargs_fut, prev).await;
        let args = args?;
        let kwargs = kwargs?;
        let text = format_print(&args, &kwargs).map_err(|msg| WeaveError::runtime(line, col, msg))?;
        let mut prints = prints.lock().expect("prints");
        let used = output_bytes.load(std::sync::atomic::Ordering::Relaxed);
        if text.len() > max_output_bytes.saturating_sub(used) {
            let _ = cancel.send(true);
            return Err(WeaveError::budget("aggregate output byte budget exceeded"));
        }
        output_bytes.store(used + text.len(), std::sync::atomic::Ordering::Relaxed);
        prints.push(text);
        Ok(Value::Null)
    });
    g.print_barrier = fut.clone();
    fut
}

fn schedule_tool(
    line: usize,
    col: usize,
    name: &str,
    arg_futs: Vec<ValFut>,
    kw_futs: Vec<(String, ValFut)>,
    g: &mut Launch,
) -> ValFut {
    let Some(tool) = g.tools.get(name) else {
        return fail(WeaveError::runtime(line, col, format!("unknown tool `{name}`")));
    };
    let effect = tool.spec.effect;
    let writes = tool.spec.writes.clone();
    let body = tool.body.clone();
    let retries = tool.spec.effective_retries();
    let timeout = tool.spec.timeout;
    let tool_name = name.to_string();
    let mut invocation = g.path.clone();
    invocation.extend([line, col]);
    let seq = g.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mutation = g.last_mutation.clone();
    let prior = g.prior.clone();
    let observable = g.last_observable.clone();
    let complete_rx = g.complete_rx.clone();
    let mut cancel_rx = g.cancel_rx.clone();
    let mode = g.mode.clone();
    let trace = g.trace.clone();
    let cancel = g.cancel_tx.clone();
    let handle = g.handle.clone();
    let speculation = g.speculation.clone();
    let max_value_size = g.limits.max_value_size;
    let fut = share(async move {
        let args_fut = join_values(arg_futs);
        let kwargs_fut = join_kwargs(kw_futs);
        let gate = async {
            ensure_running(line, col, &cancel_rx)?;
            match effect {
                Effect::Pure => {}
                Effect::Read => {
                    mutation.await?;
                }
                Effect::Reversible => {
                    await_roots(prior.futures()).await?;
                    observable.await?;
                }
                Effect::Irreversible => {
                    wait_complete(complete_rx, &mut cancel_rx).await?;
                    await_roots(prior.futures()).await?;
                    observable.await?;
                }
            }
            ensure_running(line, col, &cancel_rx)?;
            Ok::<(), WeaveError>(())
        };
        let (args, kwargs, gate_res) = futures::future::join3(args_fut, kwargs_fut, gate).await;
        let args = args?;
        let kwargs = kwargs?;
        gate_res?;
        let mut done = None;
        let outcome = match &mode {
            RunMode::Replay(recorded) => {
                let (outcome, recorded_done) = replay_tool(line, col, &invocation, &tool_name, &args, &kwargs, recorded);
                done = recorded_done;
                outcome
            }
            RunMode::Execute => {
                execute_tool(
                    line,
                    col,
                    effect,
                    &writes,
                    &speculation,
                    &tool_name,
                    &handle,
                    &body,
                    &args,
                    &kwargs,
                    retries,
                    timeout,
                    &cancel_rx,
                )
                .await
            }
        };
        let outcome = outcome.and_then(|v| checked(line, col, max_value_size, v));
        if let Err(err) = &outcome {
            if err.is_cancelled() {
                return Err(err.clone());
            }
        }
        let (result, error) = match outcome {
            Ok(value) => (value, None),
            Err(err) => (Value::Null, Some(err.summary())),
        };
        let failure = error.clone().map(|message| tool_failure(line, col, message, &args, &kwargs));
        let recorded = trace.lock().expect("trace").record(TraceEvent {
            seq,
            line,
            invocation,
            tool: tool_name,
            effect: effect.as_str().to_string(),
            args,
            kwargs,
            result: result.clone(),
            error: error.clone(),
            done: 0,
        }, done);
        if let Err(err) = recorded { let _ = cancel.send(true); return Err(err); }
        match failure {
            Some(err) => Err(err),
            None => Ok(result),
        }
    });
    match effect {
        Effect::Pure => {}
        Effect::Read => {
            g.last_observable = join_ok(g.last_observable.clone(), fut.clone());
        }
        Effect::Reversible | Effect::Irreversible => {
            g.last_mutation = fut.clone();
            g.last_observable = fut.clone();
        }
    }
    fut
}

async fn join_values(futs: Vec<ValFut>) -> Result<Vec<Value>, WeaveError> {
    let mut out = Vec::with_capacity(futs.len());
    for item in join_all(futs).await {
        out.push(item?);
    }
    Ok(out)
}

async fn join_kwargs(futs: Vec<(String, ValFut)>) -> Result<BTreeMap<String, Value>, WeaveError> {
    let mut pairs = Vec::with_capacity(futs.len());
    for (name, fut) in futs {
        pairs.push((name, fut.await?));
    }
    let mut map = BTreeMap::new();
    for (name, value) in pairs {
        if map.insert(name.clone(), value).is_some() {
            return Err(WeaveError::runtime(0, 0, format!("keyword `{name}` was passed twice")));
        }
    }
    Ok(map)
}

fn format_print(args: &[Value], kwargs: &BTreeMap<String, Value>) -> Result<String, String> {
    for key in kwargs.keys() {
        if key != "sep" && key != "end" {
            return Err(format!("print() got an unexpected keyword `{key}`"));
        }
    }
    let sep = kwargs.get("sep").map(value::py_str).unwrap_or_else(|| " ".to_string());
    let end = kwargs.get("end").map(value::py_str).unwrap_or_else(|| "\n".to_string());
    let parts: Vec<String> = args.iter().map(value::py_str).collect();
    Ok(format!("{}{end}", parts.join(&sep)))
}

fn replay_tool(
    line: usize,
    col: usize,
    invocation: &[usize],
    tool_name: &str,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    recorded: &Replay,
) -> (Result<Value, WeaveError>, Option<u64>) {
    let mut used = recorded.used.lock().expect("replay state");
    // Stable source callsite + nested iteration path; completion order is not identity.
    let candidates = recorded.by_identity.get(&(invocation.to_vec(), tool_name.to_string()));
    let found = candidates
        .into_iter()
        .flatten()
        .copied()
        .filter(|&index| {
            let event = &recorded.events[index];
            !used[index] && event.args == args && &event.kwargs == kwargs
        })
        .min_by_key(|&index| (recorded.events[index].done, index));
    let Some(index) = found else {
        let message = match used.iter().position(|taken| !taken) {
            Some(next) => format!("replay diverged: expected tool `{}`, got `{}` or differing call identity/arguments (values omitted)", recorded.events[next].tool, tool_name),
            None => format!("replay diverged: trace has no matching call for `{tool_name}` (values omitted)"),
        };
        return (Err(WeaveError::runtime(line, col, message)), None);
    };
    used[index] = true;
    let event = &recorded.events[index];
    let done = Some(event.done);
    if let Some(message) = &event.error {
        return (Err(tool_failure(line, col, message.clone(), args, kwargs)), done);
    }
    (Ok(event.result.clone()), done)
}

fn ensure_running(line: usize, col: usize, cancel: &watch::Receiver<bool>) -> Result<(), WeaveError> {
    if *cancel.borrow() {
        Err(WeaveError::cancelled(line, col))
    } else {
        Ok(())
    }
}

async fn wait_complete(
    mut done: watch::Receiver<bool>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), WeaveError> {
    loop {
        if *cancel.borrow() {
            return Err(WeaveError::cancelled(0, 0));
        }
        if *done.borrow() {
            return Ok(());
        }
        tokio::select! {
            changed = done.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            changed = cancel.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
    if *cancel.borrow() {
        Err(WeaveError::cancelled(0, 0))
    } else {
        Ok(())
    }
}

async fn execute_tool(
    line: usize,
    col: usize,
    effect: Effect,
    writes: &crate::tools::Footprint,
    speculation: &Option<SpecCache>,
    tool_name: &str,
    handle: &Handle,
    body: &ToolBody,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    retries: u32,
    timeout: Option<std::time::Duration>,
    cancel: &watch::Receiver<bool>,
) -> Result<Value, WeaveError> {
    if matches!(effect, Effect::Pure | Effect::Read) {
        if let Some(cache) = speculation {
            if let Some(hit) = cache.take(tool_name, args, kwargs).await {
                return hit.map_err(|message| tool_failure(line, col, message, args, kwargs));
            }
            cache.miss();
        }
    } else if let Some(cache) = speculation {
        // Program order: every earlier read has finished (the write waited on
        // them). Drop overlapping cached reads before the write can change
        // what they would return.
        cache.invalidate(&writes.resolve(args, kwargs));
    }
    run_tool_attempts(line, col, tool_name, handle, body, args, kwargs, retries, timeout, cancel).await
}

pub(crate) async fn run_tool_attempts(
    line: usize,
    col: usize,
    tool_name: &str,
    handle: &Handle,
    body: &ToolBody,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    retries: u32,
    timeout: Option<std::time::Duration>,
    cancel: &watch::Receiver<bool>,
) -> Result<Value, WeaveError> {
    let attempts = retries.saturating_add(1);
    let mut last = String::from("failed");
    for attempt in 1..=attempts {
        ensure_running(line, col, cancel)?;
        let outcome = invoke_once(tool_name, handle, body, args, kwargs, timeout, cancel).await;
        match outcome {
            Ok(value) => return Ok(value),
            Err(message) => {
                last = message;
                if attempt == attempts {
                    break;
                }
            }
        }
    }
    let hint_message = if timeout.is_some() {
        format!("tool `{tool_name}` failed after {attempts} attempt(s): {last}")
    } else {
        format!("tool `{tool_name}` failed: {last}")
    };
    Err(tool_failure(line, col, hint_message, args, kwargs))
}

async fn invoke_once(
    tool_name: &str,
    handle: &Handle,
    body: &ToolBody,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    timeout: Option<std::time::Duration>,
    session: &watch::Receiver<bool>,
) -> Result<Value, String> {
    let (source, token) = CancelToken::pair(session.clone());
    let call_args = args.to_vec();
    let call_kwargs = kwargs.clone();
    let label = tool_name.to_string();
    // The timer and the tool run in a normal task. A Shared future does not
    // wake tokio timers, so the wait lives outside it.
    let task = match body {
        ToolBody::Sync(func) => {
            let func = func.clone();
            let limit = timeout;
            handle.spawn_blocking(move || invoke_sync(func, call_args, call_kwargs, limit, &label))
        }
        ToolBody::Cooperative(func) => {
            let func = func.clone();
            let limit = timeout;
            let session = session.clone();
            handle.spawn(async move {
                invoke_cooperative(func, call_args, call_kwargs, token, source, limit, session, &label).await
            })
        }
        ToolBody::Async(func) => {
            let func = func.clone();
            let limit = timeout;
            let session = session.clone();
            handle.spawn(async move {
                invoke_async(func, call_args, call_kwargs, token, source, limit, session).await
            })
        }
        ToolBody::Isolated(start) => {
            let start = start.clone();
            let limit = timeout;
            let session = session.clone();
            handle.spawn(async move {
                invoke_isolated(start, call_args, call_kwargs, limit, session, &label).await
            })
        }
    };
    match task.await {
        Ok(result) => result,
        Err(_) => Err(format!("tool `{tool_name}` panicked")),
    }
}

fn invoke_sync(
    func: ToolFn,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    limit: Option<std::time::Duration>,
    tool_name: &str,
) -> Result<Value, String> {
    if let Some(limit) = limit {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(func(args, kwargs));
        });
        match rx.recv_timeout(limit) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("timed out after {}ms", limit.as_millis()))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(format!("tool `{tool_name}` panicked"))
            }
        }
    } else {
        func(args, kwargs)
    }
}

async fn invoke_cooperative(
    func: crate::tools::CooperativeFn,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    token: CancelToken,
    source: CancelSource,
    limit: Option<std::time::Duration>,
    session: watch::Receiver<bool>,
    tool_name: &str,
) -> Result<Value, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(func(args, kwargs, token));
    });
    let timeout_sleep = async {
        if let Some(limit) = limit {
            tokio::time::sleep(limit).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let mut session_rx = session.clone();
    let session_hit = async move {
        if *session_rx.borrow() {
            return;
        }
        while session_rx.changed().await.is_ok() {
            if *session_rx.borrow() {
                return;
            }
        }
    };
    tokio::select! {
        result = rx => match result {
            Ok(value) => value,
            Err(_) => Err(format!("tool `{tool_name}` panicked")),
        },
        _ = timeout_sleep => {
            source.cancel();
            Err(format!("timed out after {}ms", limit.unwrap_or_default().as_millis()))
        }
        _ = session_hit => {
            source.cancel();
            Err("execution was cancelled".into())
        }
    }
}

async fn invoke_isolated(
    start: IsolatedStart,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    limit: Option<std::time::Duration>,
    session: watch::Receiver<bool>,
    tool_name: &str,
) -> Result<Value, String> {
    struct OwnedJob(Option<IsolatedJob>);
    impl Drop for OwnedJob {
        fn drop(&mut self) {
            if let Some(job) = &self.0 { job.kill(); }
        }
    }
    // IsolatedStart must be nonblocking; Python queues immediately. Keep a
    // cancellation guard even when the session/future is dropped mid-call.
    let mut owned = OwnedJob(Some(start(args, kwargs)?));
    let job = owned.0.as_ref().expect("owned job");
    let (tx, rx) = tokio::sync::oneshot::channel();
    let wait = job.waiter();
    std::thread::spawn(move || {
        let _ = tx.send(wait());
    });
    let timeout_sleep = async {
        if let Some(limit) = limit {
            tokio::time::sleep(limit).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let mut session_rx = session.clone();
    let session_hit = async move {
        if *session_rx.borrow() {
            return;
        }
        while session_rx.changed().await.is_ok() {
            if *session_rx.borrow() {
                return;
            }
        }
    };
    tokio::select! {
        result = rx => {
            owned.0.take();
            match result {
                Ok(value) => value,
                Err(_) => Err(format!("tool `{tool_name}` panicked")),
            }
        },
        _ = timeout_sleep => {
            Err(format!("timed out after {}ms", limit.unwrap_or_default().as_millis()))
        }
        _ = session_hit => {
            Err("execution was cancelled".into())
        }
    }
}

async fn invoke_async(
    func: crate::tools::AsyncToolFn,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    token: CancelToken,
    source: CancelSource,
    limit: Option<std::time::Duration>,
    session: watch::Receiver<bool>,
) -> Result<Value, String> {
    let work = func(args, kwargs, token.clone());
    let timeout_sleep = async {
        if let Some(limit) = limit {
            tokio::time::sleep(limit).await;
            true
        } else {
            std::future::pending::<bool>().await
        }
    };
    tokio::select! {
        result = work => result,
        _ = token.cancelled() => {
            source.cancel();
            if *session.borrow() {
                Err("execution was cancelled".into())
            } else {
                Err("timed out".into())
            }
        }
        timed_out = timeout_sleep => {
            if timed_out {
                source.cancel();
                Err(format!("timed out after {}ms", limit.unwrap_or_default().as_millis()))
            } else {
                Err("timed out".into())
            }
        }
    }
}

fn to_cmp(op: CmpOp) -> value::Cmp {
    match op {
        CmpOp::Eq => value::Cmp::Eq,
        CmpOp::NotEq => value::Cmp::NotEq,
        CmpOp::Lt => value::Cmp::Lt,
        CmpOp::LtE => value::Cmp::LtE,
        CmpOp::Gt => value::Cmp::Gt,
        CmpOp::GtE => value::Cmp::GtE,
        CmpOp::In => value::Cmp::In,
        CmpOp::NotIn => value::Cmp::NotIn,
        CmpOp::Is => value::Cmp::Is,
        CmpOp::IsNot => value::Cmp::IsNot,
    }
}

fn apply_unary(op: UnaryOp, value: &Value) -> Result<Value, String> {
    match op {
        UnaryOp::Not => Ok(Value::Bool(!value::truthy(value))),
        UnaryOp::UAdd => match value {
            Value::Int(_) | Value::Float(_) | Value::Bool(_) => Ok(value.clone()),
            other => Err(format!("unary + expects a number, got {}", value::type_name(other))),
        },
        UnaryOp::USub => value::neg(value),
        UnaryOp::Invert => value::bit_not(value),
    }
}

fn apply_bin(op: BinOp, left: &Value, right: &Value) -> Result<Value, String> {
    match op {
        BinOp::Add => value::add(left, right),
        BinOp::Sub => value::sub(left, right),
        BinOp::Mul => value::mul(left, right),
        BinOp::Div => value::true_div(left, right),
        BinOp::FloorDiv => value::floor_div(left, right),
        BinOp::Mod => value::modulo(left, right),
        BinOp::Pow => value::pow(left, right),
        BinOp::BitAnd => value::bit_op("&", left, right),
        BinOp::BitOr => value::bit_op("|", left, right),
        BinOp::BitXor => value::bit_op("^", left, right),
        BinOp::LShift => value::bit_op("<<", left, right),
        BinOp::RShift => value::bit_op(">>", left, right),
    }
}

/// `parse_json(text)`: plain JSON text to a Weave value. Tool results from MCP
/// servers are text, so scripts need this to read structured results.
fn parse_json(args: &[Value]) -> Result<Value, String> {
    let text = match args.first() {
        Some(Value::Str(text)) => text,
        Some(other) => return Err(format!("parse_json() expects a string, got {}", value::type_name(other))),
        None => return Err("parse_json() takes 1 argument".into()),
    };
    let json: serde_json::Value =
        serde_json::from_str(text.trim()).map_err(|err| format!("parse_json(): invalid JSON: {err}"))?;
    fn convert(json: &serde_json::Value, depth: usize) -> Result<Value, String> {
        if depth > crate::limits::MAX_DEPTH {
            return Err("parse_json(): nesting is too deep".into());
        }
        Ok(match json {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            serde_json::Value::String(s) => Value::Str(s.clone()),
            serde_json::Value::Array(items) => {
                Value::List(items.iter().map(|v| convert(v, depth + 1)).collect::<Result<_, _>>()?)
            }
            serde_json::Value::Object(map) => Value::Dict(
                map.iter()
                    .map(|(k, v)| Ok((Value::Str(k.clone()), convert(v, depth + 1)?)))
                    .collect::<Result<_, String>>()?,
            ),
        })
    }
    convert(&json, 0)
}

fn eval_builtin(name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "len" => value::len_of(args.first().ok_or("len() takes 1 argument")?),
        "abs" => {
            let v = args.first().ok_or("abs() takes 1 argument")?;
            match v {
                Value::Int(i) => Ok(Value::Int(i.saturating_abs())),
                Value::Float(f) => Ok(Value::Float(f.abs())),
                Value::Bool(b) => Ok(Value::Int(if *b { 1 } else { 0 })),
                other => Err(format!("abs() expects a number, got {}", value::type_name(other))),
            }
        }
        "str" => Ok(Value::Str(value::py_str(args.first().ok_or("str() takes 1 argument")?))),
        "bool" => Ok(Value::Bool(value::truthy(args.first().ok_or("bool() takes 1 argument")?))),
        "int" => to_int(args.first().ok_or("int() takes 1 argument")?),
        "float" => to_float(args.first().ok_or("float() takes 1 argument")?),
        "range" => make_range(args),
        "sum" => sum_of(args),
        "min" => min_max(args, true),
        "max" => min_max(args, false),
        "sorted" => sorted_list(args),
        "reversed" => reversed_list(args),
        "enumerate" => enumerate_list(args),
        "zip" => zip_lists(args),
        "any" => any_all(args, true),
        "all" => any_all(args, false),
        "list" => as_list_value(args),
        "parse_json" => parse_json(args),
        other => Err(format!("unknown builtin `{other}`")),
    }
}

fn one_list<'a>(name: &str, args: &'a [Value]) -> Result<&'a [Value], String> {
    if args.len() != 1 {
        return Err(format!("{name}() takes 1 argument"));
    }
    match &args[0] {
        Value::List(xs) => Ok(xs),
        other => Err(format!("{name}() expects a list, got {}", value::type_name(other))),
    }
}

fn sum_of(args: &[Value]) -> Result<Value, String> {
    let xs = one_list("sum", args)?;
    let mut total = Value::Int(0);
    for item in xs {
        total = value::add(&total, item)?;
    }
    Ok(total)
}

fn min_max(args: &[Value], want_min: bool) -> Result<Value, String> {
    let name = if want_min { "min" } else { "max" };
    let items: Vec<Value> = match args {
        [Value::List(xs)] => xs.clone(),
        [a, b] => vec![a.clone(), b.clone()],
        _ => return Err(format!("{name}() takes a list or two values")),
    };
    let mut best = items.first().cloned().ok_or_else(|| format!("{name}() of an empty sequence"))?;
    for item in items.iter().skip(1) {
        let is_less = value::compare(value::Cmp::Lt, item, &best).map(|v| value::truthy(&v))?;
        if want_min == is_less {
            best = item.clone();
        }
    }
    Ok(best)
}

fn sorted_list(args: &[Value]) -> Result<Value, String> {
    let mut xs = one_list("sorted", args)?.to_vec();
    for i in 1..xs.len() {
        let mut j = i;
        while j > 0 {
            let less = value::compare(value::Cmp::Lt, &xs[j], &xs[j - 1]).map(|v| value::truthy(&v))?;
            if !less {
                break;
            }
            xs.swap(j, j - 1);
            j -= 1;
        }
    }
    Ok(Value::List(xs))
}

fn reversed_list(args: &[Value]) -> Result<Value, String> {
    let mut xs = one_list("reversed", args)?.to_vec();
    xs.reverse();
    Ok(Value::List(xs))
}

fn enumerate_list(args: &[Value]) -> Result<Value, String> {
    let xs = one_list("enumerate", args)?;
    Ok(Value::List(
        xs.iter()
            .enumerate()
            .map(|(i, item)| Value::List(vec![Value::Int(i as i64), item.clone()]))
            .collect(),
    ))
}

fn zip_lists(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("zip() takes 2 lists".into());
    }
    let left = one_list("zip", &args[..1])?;
    let right = one_list("zip", &args[1..])?;
    let n = left.len().min(right.len());
    Ok(Value::List(
        (0..n)
            .map(|i| Value::List(vec![left[i].clone(), right[i].clone()]))
            .collect(),
    ))
}

fn any_all(args: &[Value], want_any: bool) -> Result<Value, String> {
    let name = if want_any { "any" } else { "all" };
    let xs = one_list(name, args)?;
    let flag = if want_any {
        xs.iter().any(value::truthy)
    } else {
        xs.iter().all(value::truthy)
    };
    Ok(Value::Bool(flag))
}

fn as_list_value(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("list() takes 1 argument".into());
    }
    match &args[0] {
        Value::List(xs) => Ok(Value::List(xs.clone())),
        Value::Str(s) => Ok(Value::List(s.chars().map(|ch| Value::Str(ch.to_string())).collect())),
        Value::Dict(items) => Ok(Value::List(items.iter().map(|(k, _)| k.clone()).collect())),
        other => Err(format!("list() cannot convert {}", value::type_name(other))),
    }
}

fn to_int(value: &Value) -> Result<Value, String> {
    match value {
        Value::Int(i) => Ok(Value::Int(*i)),
        Value::Bool(b) => Ok(Value::Int(if *b { 1 } else { 0 })),
        Value::Float(f) if f.is_finite() => Ok(Value::Int(*f as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("invalid int literal {s:?}")),
        other => Err(format!("int() cannot convert {}", value::type_name(other))),
    }
}

fn to_float(value: &Value) -> Result<Value, String> {
    match value {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(i) => Ok(Value::Float(*i as f64)),
        Value::Bool(b) => Ok(Value::Float(if *b { 1.0 } else { 0.0 })),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| format!("invalid float literal {s:?}")),
        other => Err(format!("float() cannot convert {}", value::type_name(other))),
    }
}

fn make_range(args: &[Value]) -> Result<Value, String> {
    let ints: Result<Vec<i64>, String> = args
        .iter()
        .map(|v| v.as_int().ok_or_else(|| "range() arguments must be ints".to_string()))
        .collect();
    let ints = ints?;
    let (start, stop, step) = match ints.as_slice() {
        [stop] => (0, *stop, 1),
        [start, stop] => (*start, *stop, 1),
        [start, stop, step] => (*start, *stop, *step),
        _ => return Err("range() takes 1 to 3 arguments".into()),
    };
    if step == 0 {
        return Err("range() step cannot be zero".into());
    }
    let mut out = Vec::new();
    let mut i = start;
    while if step > 0 { i < stop } else { i > stop } {
        out.push(Value::Int(i));
        if out.len() > 100_000 {
            return Err("range() is too large".into());
        }
        match i.checked_add(step) {
            Some(next) => i = next,
            None => break,
        }
    }
    Ok(Value::List(out))
}

fn call_method(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    match (recv, name) {
        (Value::Str(s), "lower") => no_args(name, args).map(|_| Value::Str(s.to_lowercase())),
        (Value::Str(s), "upper") => no_args(name, args).map(|_| Value::Str(s.to_uppercase())),
        (Value::Str(s), method @ ("strip" | "lstrip" | "rstrip")) => {
            // Python: no argument (or None) strips whitespace; a string strips
            // any of its characters.
            let chars: Option<Vec<char>> = match args {
                [] | [Value::Null] => None,
                [Value::Str(set)] => Some(set.chars().collect()),
                [_] => return Err(format!("{method}() argument must be str or None")),
                _ => return Err(format!("{method}() takes at most 1 argument")),
            };
            let matches = |c: char| match &chars {
                None => c.is_whitespace(),
                Some(set) => set.contains(&c),
            };
            let out = match method {
                "strip" => s.trim_matches(matches),
                "lstrip" => s.trim_start_matches(matches),
                _ => s.trim_end_matches(matches),
            };
            Ok(Value::Str(out.to_string()))
        }
        (Value::Str(s), method @ ("split" | "rsplit")) => {
            if args.len() > 2 {
                return Err(format!("{method}() takes at most 2 arguments"));
            }
            let maxsplit = match args.get(1) {
                None => None,
                Some(Value::Int(n)) if *n < 0 => None,
                Some(Value::Int(n)) => Some(*n as usize),
                Some(_) => return Err(format!("{method}() maxsplit must be int")),
            };
            let sep = match args.first() {
                None | Some(Value::Null) => None,
                Some(Value::Str(sep)) if sep.is_empty() => {
                    return Err(format!("{method}() separator must not be empty"))
                }
                Some(Value::Str(sep)) => Some(sep.as_str()),
                Some(_) => return Err(format!("{method}() separator must be str or None")),
            };
            let parts = split_str(s, sep, maxsplit, method == "rsplit");
            Ok(Value::List(parts.into_iter().map(Value::str).collect()))
        }
        (Value::Str(s), "startswith") => {
            let prefix = one_str("startswith", args)?;
            Ok(Value::Bool(s.starts_with(&prefix)))
        }
        (Value::Str(s), "endswith") => {
            let suffix = one_str("endswith", args)?;
            Ok(Value::Bool(s.ends_with(&suffix)))
        }
        (Value::Str(s), "splitlines") => no_args(name, args)
            .map(|_| Value::List(s.lines().map(|line| Value::Str(line.to_string())).collect())),
        (Value::Str(s), "find") => {
            let needle = one_str("find", args)?;
            Ok(Value::Int(s.find(&needle).map(|i| s[..i].chars().count() as i64).unwrap_or(-1)))
        }
        (Value::Str(s), "count") => {
            let needle = one_str("count", args)?;
            if needle.is_empty() {
                return Err("count() argument must not be empty".into());
            }
            Ok(Value::Int(s.matches(&needle).count() as i64))
        }
        (Value::Str(s), "isalnum") => no_args(name, args).map(|_| Value::Bool(!s.is_empty() && s.chars().all(char::is_alphanumeric))),
        (Value::Str(s), "isspace") => no_args(name, args).map(|_| Value::Bool(!s.is_empty() && s.chars().all(char::is_whitespace))),
        (Value::Str(s), "isupper") => no_args(name, args).map(|_| {
            Value::Bool(s.chars().any(char::is_alphabetic) && !s.chars().any(char::is_lowercase))
        }),
        (Value::Str(s), "islower") => no_args(name, args).map(|_| {
            Value::Bool(s.chars().any(char::is_alphabetic) && !s.chars().any(char::is_uppercase))
        }),
        (Value::Str(s), "isdigit") => no_args(name, args).map(|_| Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))),
        (Value::Str(s), "isalpha") => no_args(name, args).map(|_| Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_alphabetic()))),
        (Value::Str(s), "title") => no_args(name, args).map(|_| {
            let mut out = String::new();
            let mut boundary = true;
            for ch in s.chars() {
                if ch.is_alphabetic() {
                    if boundary {
                        out.extend(ch.to_uppercase());
                    } else {
                        out.extend(ch.to_lowercase());
                    }
                    boundary = false;
                } else {
                    out.push(ch);
                    boundary = true;
                }
            }
            Value::Str(out)
        }),
        (Value::Str(s), "replace") => {
            if args.len() != 2 {
                return Err("replace() takes 2 arguments".into());
            }
            let old = args[0].as_str().ok_or("replace() old must be str")?;
            let new = args[1].as_str().ok_or("replace() new must be str")?;
            let hits = if old.is_empty() { s.chars().count() + 1 } else { s.matches(old).count() };
            let projected = (s.len() as u128) + (hits as u128) * (new.len() as u128);
            if projected > crate::limits::HARD_MAX_VALUE_SIZE as u128 {
                return Err(format!("replace() would build a string of {projected} bytes"));
            }
            Ok(Value::Str(s.replace(old, new)))
        }
        (Value::Str(s), "join") => {
            if args.len() != 1 {
                return Err("join() takes 1 argument".into());
            }
            let Value::List(xs) = &args[0] else {
                return Err("join() expects a list of strings".into());
            };
            let mut parts = Vec::with_capacity(xs.len());
            let mut projected = (s.len() as u128) * (xs.len().saturating_sub(1) as u128);
            for item in xs {
                let part = item.as_str().ok_or("join() expects a list of strings")?;
                projected += part.len() as u128;
                parts.push(part);
            }
            if projected > crate::limits::HARD_MAX_VALUE_SIZE as u128 {
                return Err(format!("join() would build a string of {projected} bytes"));
            }
            Ok(Value::Str(parts.join(s)))
        }
        (Value::Dict(items), "get") => {
            if args.is_empty() || args.len() > 2 {
                return Err("get() takes 1 or 2 arguments".into());
            }
            let default = args.get(1).cloned().unwrap_or(Value::Null);
            Ok(value::dict_get(items, &args[0]).cloned().unwrap_or(default))
        }
        (Value::Dict(items), "keys") => {
            no_args(name, args)?;
            Ok(Value::List(items.iter().map(|(k, _)| k.clone()).collect()))
        }
        (Value::Dict(items), "values") => {
            no_args(name, args)?;
            Ok(Value::List(items.iter().map(|(_, v)| v.clone()).collect()))
        }
        (Value::Dict(items), "items") => {
            no_args(name, args)?;
            Ok(Value::List(
                items
                    .iter()
                    .map(|(k, v)| Value::List(vec![k.clone(), v.clone()]))
                    .collect(),
            ))
        }
        (Value::Dict(items), "copy") => {
            no_args(name, args)?;
            Ok(Value::Dict(items.clone()))
        }
        (Value::List(xs), "count") => {
            if args.len() != 1 {
                return Err("count() takes 1 argument".into());
            }
            Ok(Value::Int(xs.iter().filter(|item| *item == &args[0]).count() as i64))
        }
        (Value::List(xs), "index") => {
            if args.len() != 1 {
                return Err("index() takes 1 argument".into());
            }
            xs.iter()
                .position(|item| item == &args[0])
                .map(|i| Value::Int(i as i64))
                .ok_or_else(|| format!("{} is not in the list", value::py_repr(&args[0])))
        }
        (Value::List(xs), "copy") => {
            no_args(name, args)?;
            Ok(Value::List(xs.clone()))
        }
        (other, method) => Err(format!(
            "{} has no method `{method}`",
            value::type_name(other)
        )),
    }
}

fn no_args(name: &str, args: &[Value]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("{name}() takes no arguments"))
    }
}

fn one_str(name: &str, args: &[Value]) -> Result<String, String> {
    if args.len() != 1 {
        return Err(format!("{name}() takes 1 argument"));
    }
    args[0]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{name}() expects str"))
}

/// Python's `str.split` / `str.rsplit` with an optional separator and maxsplit.
fn split_str(s: &str, sep: Option<&str>, maxsplit: Option<usize>, reverse: bool) -> Vec<String> {
    let limit = maxsplit.unwrap_or(usize::MAX);
    match sep {
        Some(sep) => {
            let mut parts: Vec<String> = if limit == usize::MAX {
                s.split(sep).map(str::to_string).collect()
            } else if reverse {
                s.rsplitn(limit + 1, sep).map(str::to_string).collect()
            } else {
                s.splitn(limit + 1, sep).map(str::to_string).collect()
            };
            if reverse && limit != usize::MAX {
                parts.reverse();
            }
            parts
        }
        None => {
            let mut parts = Vec::new();
            if reverse {
                let mut rest = s.trim_end();
                while !rest.is_empty() {
                    if parts.len() == limit {
                        parts.push(rest.to_string());
                        break;
                    }
                    match rest.rfind(char::is_whitespace) {
                        Some(i) => {
                            let ws = rest[i..].chars().next().map_or(1, char::len_utf8);
                            parts.push(rest[i + ws..].to_string());
                            rest = rest[..i].trim_end();
                        }
                        None => {
                            parts.push(rest.to_string());
                            break;
                        }
                    }
                }
                parts.reverse();
            } else {
                let mut rest = s.trim_start();
                while !rest.is_empty() {
                    if parts.len() == limit {
                        parts.push(rest.to_string());
                        break;
                    }
                    match rest.find(char::is_whitespace) {
                        Some(i) => {
                            parts.push(rest[..i].to_string());
                            rest = rest[i..].trim_start();
                        }
                        None => {
                            parts.push(rest.to_string());
                            break;
                        }
                    }
                }
            }
            parts
        }
    }
}

/// Folds keyword arguments into positional ones for the methods that accept
/// them (`split`/`rsplit` take `sep` and `maxsplit`, as in Python).
fn method_keywords(attr: &str, mut args: Vec<Value>, kwargs: BTreeMap<String, Value>) -> Result<Vec<Value>, String> {
    let params: &[&str] = match attr {
        "split" | "rsplit" => &["sep", "maxsplit"],
        _ => return Err(format!("method `{attr}` does not take keyword arguments")),
    };
    let mut slots: Vec<Option<Value>> = args.drain(..).map(Some).collect();
    if slots.len() > params.len() {
        return Err(format!("{attr}() takes at most {} arguments", params.len()));
    }
    slots.resize(params.len(), None);
    for (key, value) in kwargs {
        let Some(i) = params.iter().position(|p| *p == key) else {
            return Err(format!("{attr}() got an unexpected keyword argument `{key}`"));
        };
        if slots[i].is_some() {
            return Err(format!("{attr}() got multiple values for argument `{key}`"));
        }
        slots[i] = Some(value);
    }
    let defaults = [Value::Null, Value::Int(-1)];
    let last = slots.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
    Ok(slots
        .into_iter()
        .take(last)
        .enumerate()
        .map(|(i, v)| v.unwrap_or_else(|| defaults[i].clone()))
        .collect())
}

/// A tool failure. When an argument is a single character, the usual cause
/// is `for x in text` over a text result, so the hint says so.
fn tool_failure(
    line: usize,
    col: usize,
    message: String,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
) -> WeaveError {
    let mut diagnostic = crate::error::Diagnostic::tool(line, col, message);
    let single_char = args
        .iter()
        .chain(kwargs.values())
        .any(|v| matches!(v, Value::Str(s) if s.chars().count() == 1));
    if single_char {
        diagnostic = diagnostic.with_hint(
            "an argument is a single character: `for x in text` loops over characters; \
             loop over text.splitlines() to get lines",
        );
    }
    WeaveError::one(diagnostic)
}
