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
}

#[pymethods]
impl PyEngine {
    /// `worker_pool` starts that many Python interpreters. Tools registered
    /// with `hard_deadline=True` run there, and a timeout SIGKILLs the worker.
    #[new]
    #[pyo3(signature = (worker_pool=None))]
    fn new(py: Python<'_>, worker_pool: Option<usize>) -> PyResult<Self> {
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
                Some(module.getattr("Pool")?.call1((size,))?.unbind())
            }
        };
        Ok(Self {
            inner: Engine::new(),
            pool,
        })
    }

    /// Register a host tool.
    ///
    /// `effect` defaults to `irreversible`. Untagged tools are irreversible.
    /// `arity`, when set, is checked before the script runs.
    ///
    /// `reads` / `writes` name the resources the tool touches (a list of
    /// strings, or `"*"`). They default to every resource, so any write drops
    /// every speculated read. Narrow them to keep unrelated cached reads.
    #[pyo3(signature = (name, func, effect=None, arity=None, timeout_ms=None, retries=None, hard_deadline=false, reads=None, writes=None))]
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
        hard_deadline: bool,
        reads: Option<Vec<String>>,
        writes: Option<Vec<String>>,
    ) -> PyResult<()> {
        let effect = match effect.as_deref() {
            None => Effect::Irreversible,
            Some(text) => Effect::parse(text).ok_or_else(|| {
                PyValueError::new_err(format!(
                    "unknown effect `{text}`; use pure, read, reversible, or irreversible"
                ))
            })?,
        };
        let mut spec = ToolSpec::effect(effect);
        if let Some(reads) = reads {
            spec = spec.with_reads(Footprint::parse(&reads.join(",")));
        }
        if let Some(writes) = writes {
            spec = spec.with_writes(Footprint::parse(&writes.join(",")));
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
        if hard_deadline {
            if pass_cancel {
                return Err(PyValueError::new_err(
                    "hard_deadline runs the tool in a worker process; pass cancel by polling it in-process, or drop the cancel parameter",
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
            self.inner.register_isolated(&name, spec, move |args, kwargs| {
                start_isolated(&pool, &func, &args, &kwargs)
            });
        } else if pass_cancel {
            self.inner.register_cooperative(&name, spec, move |args, kwargs, token| {
                match Python::try_attach(|py| call_python(py, &func, &args, &kwargs, Some(token))) {
                    Some(result) => result.map_err(|err| err.to_string()),
                    None => Err("Python interpreter is unavailable".to_string()),
                }
            });
        } else {
            self.inner.register(&name, spec, move |args, kwargs| {
                match Python::try_attach(|py| call_python(py, &func, &args, &kwargs, None)) {
                    Some(result) => result.map_err(|err| err.to_string()),
                    None => Err("Python interpreter is unavailable".to_string()),
                }
            });
        }
        Ok(())
    }

    /// Set resource limits for later runs. `max_steps` counts statements and
    /// loop iterations; `max_value_size` counts string bytes and container
    /// elements. Omitted values keep their current setting.
    #[pyo3(signature = (max_steps=None, max_value_size=None))]
    fn set_limits(&mut self, max_steps: Option<u64>, max_value_size: Option<usize>) {
        let mut limits = self.inner.limits();
        if let Some(steps) = max_steps {
            limits = limits.with_max_steps(steps);
        }
        if let Some(size) = max_value_size {
            limits = limits.with_max_value_size(size);
        }
        self.inner.set_limits(limits);
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
    #[pyo3(signature = (effect=None, arity=None, timeout_ms=None, retries=None, hard_deadline=false))]
    #[allow(clippy::too_many_arguments)]
    fn declare(
        slf: Bound<'_, Self>,
        py: Python<'_>,
        effect: Option<String>,
        arity: Option<usize>,
        timeout_ms: Option<u64>,
        retries: Option<u32>,
        hard_deadline: bool,
    ) -> PyResult<Py<EngineDecl>> {
        Py::new(
            py,
            EngineDecl {
                engine: slf.unbind(),
                effect,
                arity,
                timeout_ms,
                retries,
                hard_deadline,
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

    fn finish(&mut self, py: Python<'_>) -> PyResult<PyRun> {
        let inner = &mut self.inner;
        let result = py.detach(|| inner.finish());
        result.map(PyRun::from_result).map_err(weave_err)
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
    hard_deadline: bool,
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
            None,
            None,
        )?;
        Ok(func)
    }
}

#[pyclass]
struct MarkDecl {
    effect: Option<String>,
    arity: Option<usize>,
    timeout_ms: Option<u64>,
    retries: Option<u32>,
    hard_deadline: bool,
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
        func.setattr(py, "__weave__", meta)?;
        Ok(func)
    }
}

#[pyfunction]
#[pyo3(signature = (effect=None, arity=None, timeout_ms=None, retries=None, hard_deadline=false))]
#[allow(clippy::too_many_arguments)]
fn declare(
    effect: Option<String>,
    arity: Option<usize>,
    timeout_ms: Option<u64>,
    retries: Option<u32>,
    hard_deadline: bool,
) -> MarkDecl {
    MarkDecl {
        effect,
        arity,
        timeout_ms,
        retries,
        hard_deadline,
    }
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
        dict_bool(meta, "hard_deadline")?,
        None,
        None,
    )
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
            move || match Python::try_attach(|py| read_job(py, &wait_job)) {
                Some(Ok(value)) => Ok(value),
                Some(Err(err)) => Err(err.to_string()),
                None => Err("Python interpreter is unavailable".into()),
            },
            move || {
                Python::try_attach(|py| {
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
) -> PyResult<Value> {
    let mut py_args = Vec::with_capacity(args.len());
    for arg in args {
        py_args.push(value_to_py(py, arg)?);
    }
    let tuple = PyTuple::new(py, py_args)?;
    let dict = PyDict::new(py);
    let mut has_kw = false;
    for (key, value) in kwargs {
        dict.set_item(key, value_to_py(py, value)?)?;
        has_kw = true;
    }
    if let Some(token) = token {
        dict.set_item("cancel", Py::new(py, PyCancelToken { inner: token })?)?;
        has_kw = true;
    }
    let result = if has_kw {
        func.call(py, &tuple, Some(&dict))?
    } else {
        func.call1(py, &tuple)?
    };
    py_to_value(&result.into_bound(py))
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
            out.push(py_to_value(&item)?);
        }
        return Ok(Value::List(out));
    }
    if let Ok(tuple) = obj.cast::<PyTuple>() {
        let mut out = Vec::with_capacity(tuple.len());
        for item in tuple.iter() {
            out.push(py_to_value(&item)?);
        }
        return Ok(Value::List(out));
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut out = Vec::with_capacity(dict.len());
        for (key, value) in dict.iter() {
            out.push((py_to_value(&key)?, py_to_value(&value)?));
        }
        return Ok(Value::Dict(out));
    }
    Err(PyTypeError::new_err(format!(
        "cannot convert {} to a Weave value",
        obj.get_type().name()?
    )))
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCancelToken>()?;
    m.add_class::<PyEngine>()?;
    m.add_class::<PySession>()?;
    m.add_class::<PyRun>()?;
    m.add_class::<PySpecCache>()?;
    m.add_function(wrap_pyfunction!(declare, m)?)?;
    Ok(())
}
