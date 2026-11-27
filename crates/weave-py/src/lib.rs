use std::collections::BTreeMap;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use weave_core::{
    CancelToken, Effect, Engine, Footprint, IsolatedJob, PredictedCall, Session, SpecCache, ToolSpec, Value,
};

#[pyclass(name = "Engine")]
struct PyEngine {
    inner: Engine,
    pool: Option<Py<PyAny>>,
    isolate_all: bool,
    debug: bool,
    isolated_tools: Vec<String>,
    in_process_tools: Vec<String>,
}

#[pymethods]
impl PyEngine {
    /// `worker_pool` starts that many Python worker processes. Tools registered
    /// with `hard_deadline=True` run there, and a timeout, run deadline or
    /// cancel kills the worker (its whole process group on POSIX).
    ///
    /// `isolate_all=True` makes worker isolation the default for every tool
    /// registered on this engine; a tool must then opt out explicitly with
    /// `hard_deadline=False`. `worker_limits` applies OS limits inside each
    /// worker (see `weave.workers.WorkerLimits`); unsupported limits fail here.
    /// `debug=True` lets host exception text and tracebacks reach scripts.
    #[new]
    #[pyo3(signature = (worker_pool=None, *, isolate_all=false, worker_limits=None, max_result_bytes=None, debug=false))]
    fn new(
        py: Python<'_>,
        worker_pool: Option<usize>,
        isolate_all: bool,
        worker_limits: Option<Py<PyAny>>,
        max_result_bytes: Option<usize>,
        debug: bool,
    ) -> PyResult<Self> {
        if worker_pool.is_none() && (isolate_all || worker_limits.is_some() || max_result_bytes.is_some()) {
            return Err(PyValueError::new_err(
                "isolate_all, worker_limits and max_result_bytes need Engine(worker_pool=N)",
            ));
        }
        let pool = match worker_pool {
            None => None,
            Some(0) => {
                return Err(PyValueError::new_err(
                    "worker_pool size must be at least 1",
                ))
            }
            Some(size) => {
                let module = py.import("weave.workers").map_err(|_| {
                    PyValueError::new_err(
                        "weave.workers is not installed; reinstall the weave wheel",
                    )
                })?;
                let kwargs = PyDict::new(py);
                if let Some(limits) = &worker_limits {
                    kwargs.set_item("limits", limits.bind(py))?;
                }
                if let Some(bytes) = max_result_bytes {
                    kwargs.set_item("max_result_bytes", bytes)?;
                }
                kwargs.set_item("debug", debug)?;
                Some(module.getattr("Pool")?.call((size,), Some(&kwargs))?.unbind())
            }
        };
        Ok(Self {
            inner: Engine::new(),
            pool,
            isolate_all,
            debug,
            isolated_tools: Vec::new(),
            in_process_tools: Vec::new(),
        })
    }

    /// What isolation this engine enforces: the worker pool (kill mechanism,
    /// applied OS limits, result cap) and which tools run in workers vs in-process.
    fn isolation<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);
        out.set_item("isolate_all", self.isolate_all)?;
        out.set_item("debug", self.debug)?;
        match &self.pool {
            Some(pool) => out.set_item("pool", pool.call_method0(py, "describe")?)?,
            None => out.set_item("pool", py.None())?,
        }
        out.set_item("isolated_tools", self.isolated_tools.clone())?;
        out.set_item("in_process_tools", self.in_process_tools.clone())?;
        Ok(out)
    }

    /// Register a host tool.
    ///
    /// `effect` defaults to `irreversible`. Untagged tools are irreversible.
    /// `arity`, when set, is checked before the script runs.
    ///
    /// `reads` / `writes` name the resources the tool touches (a list of
    /// strings, or `"*"`). They default to every resource, so any write drops
    /// every speculated read. Narrow them to keep unrelated cached reads.
    ///
    /// `returns` declares the result: a JSON Schema dict or a shape string
    /// like `'{"id": str, "items": list[{"sku": str}], "note": str?}'`.
    /// Lookups on the result that can't work under it (a missing key on a
    /// closed object, a string key on a list) are rejected before the script
    /// runs.
    ///
    /// `error_key` is for tools that report failure in their result instead
    /// of raising: a dict result with this key (and a value other than
    /// `None`) raises `ToolError` with that value as the message, so the
    /// script stops at that call or its `except` runs.
    #[pyo3(signature = (name, func, effect=None, arity=None, timeout_ms=None, retries=None, hard_deadline=None, reads=None, writes=None, retry_safe=false, check_args=true, returns=None, error_key=None))]
    #[allow(clippy::too_many_arguments)]
    fn tool(
        &mut self,
        py: Python<'_>,
        name: String,
        func: Py<PyAny>,
        effect: Option<String>,
        arity: Option<usize>,
        timeout_ms: Option<u64>,
        retries: Option<u32>,
        hard_deadline: Option<bool>,
        reads: Option<Vec<String>>,
        writes: Option<Vec<String>>,
    retry_safe: bool,
        check_args: bool,
        returns: Option<Bound<'_, PyAny>>,
        error_key: Option<String>,
    ) -> PyResult<()> {
        let effect = match effect.as_deref() {
            None => Effect::Irreversible,
            Some(text) => Effect::parse(text).ok_or_else(|| {
                PyValueError::new_err(format!(
                    "unknown effect `{text}`; use pure, read, reversible, or irreversible"
                ))
            })?,
        };
        let mut spec = ToolSpec::effect(effect).with_retry_safe(retry_safe);
        spec.accepts_kwargs = accepts_keywords(py, &func).unwrap_or(false);
        if check_args {
            if let Some(params) = signature_params(py, &func) {
                spec = spec.with_params(params);
            }
        }
        if let Some(schema) = returns_schema(py, returns.as_ref())? {
            spec = spec.with_returns(schema);
        }
        if let Some(key) = error_key_arg(error_key)? {
            spec = spec.with_error_key(key);
        }
        if let Some(reads) = reads {
            spec = spec.with_reads(footprint_from(py, &func, reads));
        }
        if let Some(writes) = writes {
            spec = spec.with_writes(footprint_from(py, &func, writes));
        }
        if let Some(arity) = arity {
            spec = spec.with_arity(arity);
        }
        if let Some(retries) = retries {
            spec = spec.with_retries(retries);
        }
        if let Some(timeout_ms) = timeout_ms {
            spec = spec.with_timeout(std::time::Duration::from_millis(timeout_ms));
        }
        let pass_cancel = wants_cancel(py, &func).unwrap_or(false);
        let isolated = hard_deadline.unwrap_or(self.isolate_all);
        if isolated {
            if pass_cancel {
                return Err(PyValueError::new_err(
                    "this tool would run in a worker process, which cannot receive a cancel token; drop the cancel parameter, or register it with hard_deadline=False to run in-process",
                ));
            }
            let pool = self
                .pool
                .as_ref()
                .ok_or_else(|| {
                    PyValueError::new_err(
                        "hard_deadline needs Engine(worker_pool=N) so a timeout can kill the worker",
                    )
                })?
                .clone_ref(py);
            pool.call_method1(py, "check_sendable", (func.bind(py),))?;
            self.isolated_tools.retain(|n| n != &name);
            self.in_process_tools.retain(|n| n != &name);
            self.isolated_tools.push(name.clone());
            self.inner.register_isolated(&name, spec, move |args, kwargs| {
                start_isolated(&pool, &func, &args, &kwargs)
            });
        } else if pass_cancel {
            self.isolated_tools.retain(|n| n != &name);
            self.in_process_tools.retain(|n| n != &name);
            self.in_process_tools.push(name.clone());
            let debug = self.debug;
            self.inner.register_cooperative(&name, spec, move |args, kwargs, token| {
                match attach_tool_thread(|py| call_python(py, &func, &args, &kwargs, Some(token), debug)) {
                    Some(result) => result,
                    None => Err("Python interpreter is unavailable".to_string()),
                }
            });
        } else {
            self.isolated_tools.retain(|n| n != &name);
            self.in_process_tools.retain(|n| n != &name);
            self.in_process_tools.push(name.clone());
            let debug = self.debug;
            self.inner.register(&name, spec, move |args, kwargs| {
                match attach_tool_thread(|py| call_python(py, &func, &args, &kwargs, None, debug)) {
                    Some(result) => result,
                    None => Err("Python interpreter is unavailable".to_string()),
                }
            });
        }
        Ok(())
    }

    /// Set resource limits for later runs. `max_steps` counts statements and
    /// loop iterations; `max_value_size` counts string bytes and container
    /// elements. Omitted values keep their current setting.
    #[pyo3(signature = (max_steps=None, max_value_size=None, max_wall_time_ms=None, max_source_bytes=None, max_output_bytes=None, max_trace_bytes=None, pipeline_loops=None))]
    #[allow(clippy::too_many_arguments)]
    fn set_limits(&mut self, max_steps: Option<u64>, max_value_size: Option<usize>, max_wall_time_ms: Option<u64>, max_source_bytes: Option<usize>, max_output_bytes: Option<usize>, max_trace_bytes: Option<usize>, pipeline_loops: Option<bool>) {
        let mut limits = self.inner.limits();
        if let Some(steps) = max_steps {
            limits = limits.with_max_steps(steps);
        }
        if let Some(size) = max_value_size {
            limits = limits.with_max_value_size(size);
        }
        if let Some(ms) = max_wall_time_ms {
            limits = limits.with_max_wall_time(std::time::Duration::from_millis(ms));
        }
        if let Some(n) = max_source_bytes { limits.max_source_bytes = n; }
        if let Some(n) = max_output_bytes { limits.max_output_bytes = n; }
        if let Some(n) = max_trace_bytes { limits.max_trace_bytes = n; }
        if let Some(on) = pipeline_loops { limits.pipeline_loops = on; }
        self.inner.set_limits(limits);
    }

    /// "pure", "read", "reversible" or "irreversible": the strongest effect
    /// of any call in the script (unknown names count as irreversible).
    fn script_effect(&self, source: &str) -> PyResult<String> {
        self.inner.script_effect(source).map(|e| e.as_str().to_string()).map_err(weave_err)
    }

    /// The static plan of a script, without running anything: a dict with
    /// `calls` (one entry per tool call site: tool, effect, line, min_count,
    /// max_count, args with their sources, data_from, control_from, reads,
    /// writes, stage), `min_calls`, `max_calls`, `stages`, `max_effect`,
    /// `may_write`, and `text`, a short human-readable summary. A script
    /// that fails the static check raises the same error as `run`.
    fn plan(&self, py: Python<'_>, source: &str) -> PyResult<Py<PyAny>> {
        let plan = self.inner.plan(source).map_err(weave_err)?;
        let json = py.import("json")?;
        let dict = json.call_method1("loads", (plan.to_json(),))?;
        dict.set_item("text", plan.render())?;
        Ok(dict.unbind())
    }

    fn check(&self, source: &str) -> Vec<String> {
        self.inner.check(source).into_iter().map(|d| d.to_string()).collect()
    }

    #[pyo3(signature = (source, speculation=None))]
    fn run(
        &self,
        py: Python<'_>,
        source: &str,
        speculation: Option<Py<PySpecCache>>,
    ) -> PyResult<PyRun> {
        // Release the GIL so tool threads can enter Python (time.sleep does).
        let source = source.to_string();
        let cache = speculation
            .as_ref()
            .map(|spec| spec.bind(py).borrow().inner.clone());
        let result = py.detach(|| self.inner.run_with(&source, cache.as_ref()));
        result.map(PyRun::from_result).map_err(weave_err)
    }

    /// Pre-run pure and read calls. `calls` is a list of `(name, args)` or
    /// `(name, args, kwargs)`. The cache key is the tool name plus those
    /// arguments. Reversible and irreversible tools are rejected.
    fn speculate(&self, py: Python<'_>, calls: &Bound<'_, PyAny>) -> PyResult<Py<PySpecCache>> {
        let mut predicted = Vec::new();
        for item in calls.try_iter()? {
            predicted.push(parse_predicted(py, &item?)?);
        }
        let cache = self.inner.speculate(&predicted);
        Py::new(py, PySpecCache { inner: cache })
    }

    /// `predictor.predict(hint)` returns the same list shape as `speculate`.
    fn speculate_with(
        &mut self,
        py: Python<'_>,
        predictor: &Bound<'_, PyAny>,
        hint: &str,
    ) -> PyResult<Py<PySpecCache>> {
        let calls = predictor.call_method1("predict", (hint,))?;
        self.speculate(py, &calls)
    }

    /// Register every function previously marked with `weave.declare`.
    fn install(&mut self, py: Python<'_>, funcs: &Bound<'_, PyAny>) -> PyResult<()> {
        for item in funcs.try_iter()? {
            install_marked(self, py, &item?)?;
        }
        Ok(())
    }

    /// Decorator that registers the function on this engine.
    #[pyo3(signature = (effect=None, arity=None, timeout_ms=None, retries=None, hard_deadline=None, reads=None, writes=None, retry_safe=false, check_args=true, returns=None, error_key=None))]
    #[allow(clippy::too_many_arguments)]
    fn declare(
        slf: Bound<'_, Self>,
        py: Python<'_>,
        effect: Option<String>,
        arity: Option<usize>,
        timeout_ms: Option<u64>,
        retries: Option<u32>,
        hard_deadline: Option<bool>,
        reads: Option<Vec<String>>,
        writes: Option<Vec<String>>,
    retry_safe: bool,
        check_args: bool,
        returns: Option<Py<PyAny>>,
        error_key: Option<String>,
    ) -> PyResult<Py<EngineDecl>> {
        error_key_arg(error_key.clone())?;
        Py::new(
            py,
            EngineDecl {
                engine: slf.unbind(),
                effect,
                arity,
                timeout_ms,
                retries,
                hard_deadline,
                reads,
                writes,
                retry_safe,
                check_args,
                returns,
                error_key,
            },
        )
    }

    fn replay(&self, py: Python<'_>, source: &str, trace_json: &str) -> PyResult<PyRun> {
        let trace = weave_core::Trace::from_json(trace_json).map_err(weave_err)?;
        let source = source.to_string();
        let result = py.detach(|| self.inner.replay(&source, &trace));
        result.map(PyRun::from_result).map_err(weave_err)
    }

    fn session(&self) -> PySession {
        PySession {
            inner: self.inner.session(),
        }
    }
}

#[pyclass(name = "Session", unsendable)]
struct PySession {
    inner: Session,
}

#[pymethods]
impl PySession {
    fn push(&mut self, py: Python<'_>, chunk: &str) -> PyResult<()> {
        let chunk = chunk.to_string();
        let inner = &mut self.inner;
        py.detach(|| inner.push(&chunk)).map_err(weave_err)
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    /// A handle that cancels this session from any thread (for example an
    /// asyncio task cancelled while the session runs in a worker thread).
    fn cancel_handle(&self) -> PySessionCancel {
        PySessionCancel { inner: self.inner.cancel_source() }
    }

    fn finish(&mut self, py: Python<'_>) -> PyResult<PyRun> {
        let inner = &mut self.inner;
        let result = py.detach(|| inner.finish());
        result.map(PyRun::from_result).map_err(weave_err)
    }
}

/// Thread-safe cancel for one session. Isolated tool workers are killed;
/// in-process tools are soft-cancelled (the session stops waiting).
#[pyclass(name = "SessionCancel")]
struct PySessionCancel {
    inner: weave_core::CancelSource,
}

#[pymethods]
impl PySessionCancel {
    fn cancel(&self) {
        self.inner.cancel();
    }
}

#[pyclass(name = "Run")]
struct PyRun {
    prints: Vec<String>,
    bindings: BTreeMap<String, Value>,
    trace: String,
    spec_started: u64,
    spec_hits: u64,
    spec_misses: u64,
    spec_wasted: u64,
    spec_rejected: u64,
    spec_invalidated: u64,
}

#[pymethods]
impl PyRun {
    #[getter]
    fn prints(&self) -> Vec<String> {
        self.prints.clone()
    }

    #[getter]
    fn stdout(&self) -> String {
        self.prints.concat()
    }

    #[getter]
    fn bindings<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (name, value) in &self.bindings {
            dict.set_item(name, value_to_py(py, value)?)?;
        }
        Ok(dict)
    }

    fn trace_json(&self) -> &str {
        &self.trace
    }

    #[getter]
    fn spec_started(&self) -> u64 {
        self.spec_started
    }

    #[getter]
    fn spec_hits(&self) -> u64 {
        self.spec_hits
    }

    #[getter]
    fn spec_misses(&self) -> u64 {
        self.spec_misses
    }

    #[getter]
    fn spec_wasted(&self) -> u64 {
        self.spec_wasted
    }

    #[getter]
    fn spec_rejected(&self) -> u64 {
        self.spec_rejected
    }

    #[getter]
    fn spec_invalidated(&self) -> u64 {
        self.spec_invalidated
    }
}

impl PyRun {
    fn from_result(result: weave_core::RunResult) -> Self {
        let trace = result.trace.to_json().unwrap_or_else(|_| "{}".into());
        let stats = result.speculation.unwrap_or_default();
        Self {
            prints: result.prints,
            bindings: result.bindings,
            trace,
            spec_started: stats.started,
            spec_hits: stats.hits,
            spec_misses: stats.misses,
            spec_wasted: stats.wasted,
            spec_rejected: stats.rejected,
            spec_invalidated: stats.invalidated,
        }
    }
}

impl Drop for PyEngine {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            Python::try_attach(|py| {
                let _ = pool.call_method0(py, "close");
            });
        }
    }
}

#[pyclass(name = "Speculation")]
struct PySpecCache {
    inner: SpecCache,
}

#[pyclass]
struct EngineDecl {
    engine: Py<PyEngine>,
    effect: Option<String>,
    arity: Option<usize>,
    timeout_ms: Option<u64>,
    retries: Option<u32>,
    hard_deadline: Option<bool>,
    reads: Option<Vec<String>>,
    writes: Option<Vec<String>>,
    retry_safe: bool,
    check_args: bool,
    returns: Option<Py<PyAny>>,
    error_key: Option<String>,
}

#[pymethods]
impl EngineDecl {
    fn __call__(&self, py: Python<'_>, func: Py<PyAny>) -> PyResult<Py<PyAny>> {
        let name: String = func.getattr(py, "__name__")?.extract(py)?;
        self.engine.borrow_mut(py).tool(
            py,
            name,
            func.clone_ref(py),
            self.effect.clone(),
            self.arity,
            self.timeout_ms,
            self.retries,
            self.hard_deadline,
            self.reads.clone(),
            self.writes.clone(),
            self.retry_safe,
            self.check_args,
            self.returns.as_ref().map(|r| r.bind(py).clone()),
            self.error_key.clone(),
        )?;
        if let Some(r) = &self.returns {
            set_display_returns(py, &func, r)?;
        }
        Ok(func)
    }
}

#[pyclass]
struct MarkDecl {
    returns: Option<Py<PyAny>>,
    effect: Option<String>,
    arity: Option<usize>,
    timeout_ms: Option<u64>,
    retries: Option<u32>,
    hard_deadline: Option<bool>,
    reads: Option<Vec<String>>,
    writes: Option<Vec<String>>,
    retry_safe: bool,
    check_args: bool,
    error_key: Option<String>,
}

#[pymethods]
impl MarkDecl {
    fn __call__(&self, py: Python<'_>, func: Py<PyAny>) -> PyResult<Py<PyAny>> {
        let meta = PyDict::new(py);
        meta.set_item("effect", self.effect.clone())?;
        meta.set_item("arity", self.arity)?;
        meta.set_item("timeout_ms", self.timeout_ms)?;
        meta.set_item("retries", self.retries)?;
        meta.set_item("hard_deadline", self.hard_deadline)?;
        meta.set_item("reads", self.reads.clone())?;
        meta.set_item("writes", self.writes.clone())?;
        meta.set_item("retry_safe", self.retry_safe)?;
        meta.set_item("check_args", self.check_args)?;
        meta.set_item("returns", self.returns.as_ref().map(|r| r.clone_ref(py)))?;
        meta.set_item("error_key", self.error_key.clone())?;
        if let Some(r) = &self.returns {
            // Fail at decoration time, not at install time.
            returns_schema(py, Some(r.bind(py)))?;
            set_display_returns(py, &func, r)?;
        }
        func.setattr(py, "__weave__", meta)?;
        Ok(func)
    }
}

#[pyfunction]
#[pyo3(signature = (effect=None, arity=None, timeout_ms=None, retries=None, hard_deadline=None, reads=None, writes=None, retry_safe=false, check_args=true, returns=None, error_key=None))]
#[allow(clippy::too_many_arguments)]
fn declare(
    effect: Option<String>,
    arity: Option<usize>,
    timeout_ms: Option<u64>,
    retries: Option<u32>,
    hard_deadline: Option<bool>,
    reads: Option<Vec<String>>,
    writes: Option<Vec<String>>,
    retry_safe: bool,
    check_args: bool,
    returns: Option<Py<PyAny>>,
    error_key: Option<String>,
) -> PyResult<MarkDecl> {
    error_key_arg(error_key.clone())?;
    Ok(MarkDecl {
        returns,
        effect,
        arity,
        timeout_ms,
        retries,
        hard_deadline,
        reads,
        writes,
        retry_safe,
        check_args,
        error_key,
    })
}

fn install_marked(engine: &mut PyEngine, py: Python<'_>, func: &Bound<'_, PyAny>) -> PyResult<()> {
    let meta = func.getattr("__weave__").map_err(|_| {
        PyValueError::new_err("install() expects functions decorated with weave.declare")
    })?;
    let meta = meta.cast::<PyDict>()?;
    let name: String = func.getattr("__name__")?.extract()?;
    engine.tool(
        py,
        name,
        func.clone().unbind(),
        dict_opt_string(meta, "effect")?,
        dict_opt_usize(meta, "arity")?,
        dict_opt_u64(meta, "timeout_ms")?,
        dict_opt_u32(meta, "retries")?,
        dict_opt_bool(meta, "hard_deadline")?,
        dict_opt_strings(meta, "reads")?,
        dict_opt_strings(meta, "writes")?,
        dict_bool(meta, "retry_safe")?,
        // Marked before `check_args` existed: keep the default.
        match meta.get_item("check_args")? {
            Some(value) if !value.is_none() => value.extract()?,
            _ => true,
        },
        meta.get_item("returns")?.filter(|v| !v.is_none()),
        dict_opt_string(meta, "error_key")?,
    )
}

/// `error_key=`: a non-empty key name, or None.
fn error_key_arg(key: Option<String>) -> PyResult<Option<String>> {
    match key {
        Some(k) if k.is_empty() => Err(PyValueError::new_err("error_key= must be a non-empty string")),
        other => Ok(other),
    }
}

/// `returns=`: a JSON Schema (a dict) or a shape string such as
/// `{"id": str, "items": list[{"price": float}]}`.
fn returns_schema(py: Python<'_>, returns: Option<&Bound<'_, PyAny>>) -> PyResult<Option<serde_json::Value>> {
    let Some(value) = returns.filter(|v| !v.is_none()) else { return Ok(None) };
    if let Ok(text) = value.extract::<String>() {
        return weave_core::schema_from_shape(&text).map(Some).ok_or_else(|| {
            PyValueError::new_err(format!(
                "returns={text:?} is not a shape like '{{\"id\": str, \"tags\": list[str], \"note\": str?}}'; pass a JSON Schema dict instead"
            ))
        });
    }
    if value.cast::<PyDict>().is_ok() {
        let text: String = py.import("json")?.call_method1("dumps", (value,))?.extract()?;
        let schema: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| PyValueError::new_err(format!("returns= is not JSON: {e}")))?;
        return Ok(Some(schema));
    }
    Err(PyValueError::new_err("returns= must be a JSON Schema dict or a shape string"))
}

/// Show the declared return type in tool listings, unless the function
/// already sets its own `__weave_returns__`.
fn set_display_returns(py: Python<'_>, func: &Py<PyAny>, returns: &Py<PyAny>) -> PyResult<()> {
    let f = func.bind(py);
    if f.getattr("__weave_returns__").map(|v| v.is_none()).unwrap_or(true) {
        f.setattr("__weave_returns__", returns.clone_ref(py))?;
    }
    Ok(())
}

fn dict_opt_strings(meta: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<Vec<String>>> {
    match meta.get_item(key)? {
        Some(value) if !value.is_none() => Ok(Some(value.extract()?)),
        _ => Ok(None),
    }
}

/// Rewrite `{param}` to `{index}` using the function's positional parameter
/// names, so Python footprints can say `"user:{user_id}"`.
fn footprint_from(py: Python<'_>, func: &Py<PyAny>, names: Vec<String>) -> Footprint {
    let params: Vec<String> = py
        .import("inspect")
        .and_then(|inspect| inspect.call_method1("signature", (func.bind(py),)))
        .and_then(|sig| sig.getattr("parameters"))
        .and_then(|params| params.call_method0("keys"))
        .and_then(|keys| keys.try_iter().and_then(|it| it.map(|k| k.and_then(|k| k.extract::<String>())).collect()))
        .unwrap_or_default();
    let mapped: Vec<String> = names
        .into_iter()
        .map(|name| {
            let mut out = name;
            for (index, param) in params.iter().enumerate() {
                out = out.replace(&format!("{{{param}}}"), &format!("{{{index}|{param}}}"));
            }
            out
        })
        .collect();
    Footprint::parse(&mapped.join(","))
}

fn dict_opt_string(meta: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match meta.get_item(key)? {
        Some(value) if !value.is_none() => Ok(Some(value.extract()?)),
        _ => Ok(None),
    }
}

fn dict_opt_usize(meta: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<usize>> {
    match meta.get_item(key)? {
        Some(value) if !value.is_none() => Ok(Some(value.extract()?)),
        _ => Ok(None),
    }
}

fn dict_opt_u64(meta: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<u64>> {
    match meta.get_item(key)? {
        Some(value) if !value.is_none() => Ok(Some(value.extract()?)),
        _ => Ok(None),
    }
}

fn dict_opt_u32(meta: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<u32>> {
    match meta.get_item(key)? {
        Some(value) if !value.is_none() => Ok(Some(value.extract()?)),
        _ => Ok(None),
    }
}

fn dict_opt_bool(meta: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<bool>> {
    match meta.get_item(key)? {
        Some(value) if !value.is_none() => Ok(Some(value.extract()?)),
        _ => Ok(None),
    }
}

fn dict_bool(meta: &Bound<'_, PyDict>, key: &str) -> PyResult<bool> {
    match meta.get_item(key)? {
        Some(value) if !value.is_none() => Ok(value.extract()?),
        _ => Ok(false),
    }
}

fn parse_predicted(py: Python<'_>, item: &Bound<'_, PyAny>) -> PyResult<PredictedCall> {
    let name: String = item.get_item(0)?.extract()?;
    let args_obj = item.get_item(1)?;
    let mut args = Vec::new();
    for arg in args_obj.try_iter()? {
        args.push(py_to_value(&arg?)?);
    }
    let mut kwargs = BTreeMap::new();
    if let Ok(raw) = item.get_item(2) {
        if !raw.is_none() {
            let dict = raw.cast::<PyDict>()?;
            for (key, value) in dict.iter() {
                kwargs.insert(key.extract()?, py_to_value(&value)?);
            }
        }
    }
    let _ = py;
    Ok(PredictedCall::new(name, args).with_kwargs(kwargs))
}

fn start_isolated(
    pool: &Py<PyAny>,
    func: &Py<PyAny>,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
) -> Result<IsolatedJob, String> {
    let attached = Python::try_attach(|py| -> PyResult<IsolatedJob> {
        let py_args = PyList::empty(py);
        for arg in args {
            py_args.append(value_to_py(py, arg)?)?;
        }
        let py_kwargs = PyDict::new(py);
        for (key, value) in kwargs {
            py_kwargs.set_item(key, value_to_py(py, value)?)?;
        }
        let job = pool
            .bind(py)
            .call_method1("start", (func, &py_args, &py_kwargs))?;
        let job: Py<PyAny> = job.unbind();
        let wait_job = job.clone_ref(py);
        let kill_job = job;
        Ok(IsolatedJob::new(
            move || match attach_tool_thread(|py| read_job(py, &wait_job)) {
                Some(Ok(value)) => Ok(value),
                Some(Err(err)) => Err(err.to_string()),
                None => Err("Python interpreter is unavailable".into()),
            },
            move || {
                attach_tool_thread(|py| {
                    let _ = kill_job.call_method0(py, "kill");
                });
            },
        ))
    });
    match attached {
        Some(Ok(job)) => Ok(job),
        Some(Err(err)) => Err(err.to_string()),
        None => Err("Python interpreter is unavailable".into()),
    }
}

fn read_job(py: Python<'_>, job: &Py<PyAny>) -> PyResult<Value> {
    let pair = job.call_method0(py, "wait")?.into_bound(py);
    let status: String = pair.get_item(0)?.extract()?;
    if status == "ok" {
        py_to_value(&pair.get_item(1)?)
    } else {
        let message: String = pair.get_item(1)?.extract()?;
        Err(PyValueError::new_err(message))
    }
}

fn weave_err(err: weave_core::WeaveError) -> PyErr {
    PyValueError::new_err(err.to_string())
}

fn accepts_keywords(py: Python<'_>, func: &Py<PyAny>) -> PyResult<bool> {
    let inspect = py.import("inspect")?;
    let sig = inspect.call_method1("signature", (func,))?;
    for param in sig.getattr("parameters")?.call_method0("values")?.try_iter()? {
        let param = param?;
        let name: String = param.getattr("name")?.extract()?;
        let kind: i32 = param.getattr("kind")?.extract()?;
        // POSITIONAL_OR_KEYWORD=1, KEYWORD_ONLY=3, VAR_KEYWORD=4.
        if name != "cancel" && matches!(kind, 1 | 3 | 4) { return Ok(true); }
    }
    Ok(false)
}

/// Parameters from the function's Python signature, so the static check can
/// reject an unknown keyword, a missing required argument or too many
/// positional arguments before any tool runs. Annotations are not used: Python
/// does not enforce them, so a type check could reject a call that works.
/// `cancel` is injected by the host and is not a script parameter. Returns
/// `None` when the signature cannot be read.
fn signature_params(py: Python<'_>, func: &Py<PyAny>) -> Option<weave_core::Params> {
    let inspect = py.import("inspect").ok()?;
    let sig = inspect.call_method1("signature", (func,)).ok()?;
    let empty = inspect.getattr("Parameter").ok()?.getattr("empty").ok()?;
    let mut out = weave_core::Params::default();
    for param in sig.getattr("parameters").ok()?.call_method0("values").ok()?.try_iter().ok()? {
        let param = param.ok()?;
        let name: String = param.getattr("name").ok()?.extract().ok()?;
        let kind: i32 = param.getattr("kind").ok()?.extract().ok()?;
        if name == "cancel" {
            continue;
        }
        let required = param.getattr("default").ok()?.is(&empty);
        match kind {
            // POSITIONAL_ONLY, POSITIONAL_OR_KEYWORD, KEYWORD_ONLY
            0 | 1 | 3 => out.params.push(weave_core::Param {
                name,
                required,
                positional: kind != 3,
                keyword: kind != 0,
                schema: None,
            }),
            2 => out.extra_positional = true,
            4 => out.extra_keywords = true,
            _ => return None,
        }
    }
    Some(out)
}

fn wants_cancel(py: Python<'_>, func: &Py<PyAny>) -> PyResult<bool> {
    let inspect = py.import("inspect")?;
    let sig = match inspect.call_method1("signature", (func,)) {
        Ok(sig) => sig,
        Err(_) => return Ok(false),
    };
    let params = sig.getattr("parameters")?;
    let values = params.call_method0("values")?;
    let var_keyword = inspect.getattr("Parameter")?.getattr("VAR_KEYWORD")?;
    for item in values.try_iter()? {
        let param = item?;
        let name: String = param.getattr("name")?.extract()?;
        if name == "cancel" {
            return Ok(true);
        }
        let kind = param.getattr("kind")?;
        if kind.eq(&var_keyword)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn call_python(
    py: Python<'_>,
    func: &Py<PyAny>,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    token: Option<CancelToken>,
    debug: bool,
) -> Result<Value, String> {
    let prepared = (|| -> PyResult<(Bound<'_, PyTuple>, Bound<'_, PyDict>, bool)> {
        let mut py_args = Vec::with_capacity(args.len());
        for arg in args {
            py_args.push(value_to_py(py, arg)?);
        }
        let tuple = PyTuple::new(py, py_args)?;
        let dict = PyDict::new(py);
        let mut has_kw = false;
        for (key, value) in kwargs {
            if key == "cancel" && token.is_some() {
                return Err(PyValueError::new_err("cancel is injected by the host, not a script argument"));
            }
            dict.set_item(key, value_to_py(py, value)?)?;
            has_kw = true;
        }
        if let Some(token) = token {
            dict.set_item("cancel", Py::new(py, PyCancelToken { inner: token })?)?;
            has_kw = true;
        }
        Ok((tuple, dict, has_kw))
    })()
    .map_err(|err| err.to_string())?;
    let (tuple, dict, has_kw) = prepared;
    // Errors raised by host code may carry secrets; only Weave's own argument
    // and conversion errors above and below are reported verbatim.
    let result = if has_kw { func.call(py, &tuple, Some(&dict)) } else { func.call1(py, &tuple) };
    let result = result.map_err(|err| {
        if err.is_instance_of::<PyTypeError>(py) {
            if let Some(text) = call_mismatch(py, func, &tuple, &dict) {
                return text;
            }
        }
        public_error(py, &err, debug)
    })?;
    py_to_value(&result.into_bound(py)).map_err(|err| err.to_string())
}

/// The script called the tool with arguments its signature rejects.
fn call_mismatch(py: Python<'_>, func: &Py<PyAny>, args: &Bound<'_, PyTuple>, kwargs: &Bound<'_, PyDict>) -> Option<String> {
    py.import("weave.errors")
        .and_then(|module| module.getattr("call_mismatch")?.call1((func.bind(py), args, kwargs)))
        .ok()
        .and_then(|text| text.extract::<Option<String>>().ok().flatten())
}

/// Host exception text for scripts: redacted unless `weave.ToolError` or debug.
fn public_error(py: Python<'_>, err: &PyErr, debug: bool) -> String {
    let described = py
        .import("weave.errors")
        .and_then(|module| {
            let kwargs = PyDict::new(py);
            kwargs.set_item("debug", debug)?;
            module.getattr("describe_exception")?.call((err.value(py),), Some(&kwargs))
        })
        .and_then(|text| text.extract::<String>());
    match described {
        Ok(text) => text,
        Err(_) => {
            let name = err.get_type(py).name().map(|n| n.to_string()).unwrap_or_else(|_| "Exception".into());
            format!("{name} (details redacted)")
        }
    }
}

#[pyclass(name = "CancelToken")]
struct PyCancelToken {
    inner: CancelToken,
}

#[pymethods]
impl PyCancelToken {
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

fn value_to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    match value {
        Value::Null => Ok(py.None().into_bound(py)),
        Value::Bool(v) => Ok(PyBool::new(py, *v).to_owned().into_any()),
        Value::Int(v) => Ok((*v).into_pyobject(py)?.into_any()),
        Value::Float(v) => Ok((*v).into_pyobject(py)?.into_any()),
        Value::Str(v) => Ok(PyString::new(py, v).into_any()),
        Value::List(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(value_to_py(py, item)?)?;
            }
            Ok(list.into_any())
        }
        Value::Dict(items) => {
            let dict = PyDict::new(py);
            for (key, item) in items {
                dict.set_item(value_to_py(py, key)?, value_to_py(py, item)?)?;
            }
            Ok(dict.into_any())
        }
    }
}

fn py_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    py_to_value_depth(obj, 0)
}

fn py_to_value_depth(obj: &Bound<'_, PyAny>, depth: usize) -> PyResult<Value> {
    if depth > 100 { return Err(PyValueError::new_err("Python result exceeds depth 100 (possibly cyclic)")); }
    if obj.is_none() {
        return Ok(Value::Null);
    }
    if obj.is_instance_of::<PyBool>() {
        return Ok(Value::Bool(obj.extract()?));
    }
    if obj.is_instance_of::<PyInt>() {
        let value = obj.extract::<i64>().map_err(|_| {
            PyValueError::new_err("int does not fit in i64; use a smaller integer")
        })?;
        return Ok(Value::Int(value));
    }
    if obj.is_instance_of::<PyFloat>() {
        return Ok(Value::Float(obj.extract()?));
    }
    if obj.is_instance_of::<PyString>() {
        return Ok(Value::Str(obj.extract()?));
    }
    if let Ok(list) = obj.cast::<PyList>() {
        let mut out = Vec::with_capacity(list.len());
        for item in list.iter() {
            out.push(py_to_value_depth(&item, depth + 1)?);
        }
        return Ok(Value::List(out));
    }
    if let Ok(tuple) = obj.cast::<PyTuple>() {
        let mut out = Vec::with_capacity(tuple.len());
        for item in tuple.iter() {
            out.push(py_to_value_depth(&item, depth + 1)?);
        }
        return Ok(Value::List(out));
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut out = Vec::with_capacity(dict.len());
        for (key, value) in dict.iter() {
            out.push((py_to_value_depth(&key, depth + 1)?, py_to_value_depth(&value, depth + 1)?));
        }
        return Ok(Value::Dict(out));
    }
    Err(PyTypeError::new_err(format!(
        "cannot convert {} to a Weave value",
        obj.get_type().name()?
    )))
}

/// Tool threads currently running Python code.
///
/// A timed-out or cancelled in-process tool keeps running on its own thread
/// after the run returns. If the interpreter starts shutting down while that
/// thread is still inside Python, CPython aborts the process ("PyGILState_Release:
/// thread state must be current"). An `atexit` hook waits for these threads.
static TOOL_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How long the exit hook waits for running tool threads.
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// `Python::try_attach`, counted in [`TOOL_THREADS`] for its whole duration.
fn attach_tool_thread<R>(f: impl FnOnce(Python<'_>) -> R) -> Option<R> {
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            TOOL_THREADS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    TOOL_THREADS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let _counted = Counted;
    Python::try_attach(f)
}

/// Registered with `atexit`: wait up to `timeout_s` (with the GIL released)
/// for tool threads still running Python, so a cancelled or timed-out tool
/// can return before the interpreter shuts down. Returns how many were still
/// running when it gave up (0 when all finished).
#[pyfunction]
#[pyo3(signature = (timeout_s=None))]
fn _wait_for_tool_threads(py: Python<'_>, timeout_s: Option<f64>) -> usize {
    let limit = timeout_s
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(std::time::Duration::from_secs_f64)
        .unwrap_or(EXIT_WAIT);
    py.detach(|| {
        let started = std::time::Instant::now();
        loop {
            let running = TOOL_THREADS.load(std::sync::atomic::Ordering::SeqCst);
            if running == 0 || started.elapsed() >= limit {
                return running;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    })
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCancelToken>()?;
    m.add_class::<PyEngine>()?;
    m.add_class::<PySession>()?;
    m.add_class::<PySessionCancel>()?;
    m.add_class::<PyRun>()?;
    m.add_class::<PySpecCache>()?;
    m.add_function(wrap_pyfunction!(declare, m)?)?;
    let wait = wrap_pyfunction!(_wait_for_tool_threads, m)?;
    m.add_function(wait.clone())?;
    m.py().import("atexit")?.call_method1("register", (wait,))?;
    Ok(())
}
