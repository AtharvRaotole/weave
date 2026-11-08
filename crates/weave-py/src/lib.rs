use std::collections::BTreeMap;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use weave_core::{Effect, Engine, Session, ToolSpec, Value};

#[pyclass(name = "Engine")]
struct PyEngine {
    inner: Engine,
}

#[pymethods]
impl PyEngine {
    #[new]
    fn new() -> Self {
        Self { inner: Engine::new() }
    }

    /// Register a host tool.
    ///
    /// `effect` defaults to `irreversible`. Untagged tools are irreversible.
    /// `arity`, when set, is checked before the script runs.
    #[pyo3(signature = (name, func, effect=None, arity=None))]
    fn tool(
        &mut self,
        name: String,
        func: Py<PyAny>,
        effect: Option<String>,
        arity: Option<usize>,
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
        if let Some(arity) = arity {
            spec = spec.with_arity(arity);
        }
        self.inner.register(&name, spec, move |args, kwargs| {
            match Python::try_attach(|py| call_python(py, &func, &args, &kwargs)) {
                Some(result) => result.map_err(|err| err.to_string()),
                None => Err("Python interpreter is unavailable".to_string()),
            }
        });
        Ok(())
    }

    fn check(&self, source: &str) -> Vec<String> {
        self.inner.check(source).into_iter().map(|d| d.to_string()).collect()
    }

    fn run(&self, py: Python<'_>, source: &str) -> PyResult<PyRun> {
        // Release the GIL so tool threads can enter Python (time.sleep does).
        let source = source.to_string();
        let result = py.detach(|| self.inner.run(&source));
        result.map(PyRun::from_result).map_err(weave_err)
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
}

impl PyRun {
    fn from_result(result: weave_core::RunResult) -> Self {
        let trace = result.trace.to_json().unwrap_or_else(|_| "{}".into());
        Self {
            prints: result.prints,
            bindings: result.bindings,
            trace,
        }
    }
}

fn weave_err(err: weave_core::WeaveError) -> PyErr {
    PyValueError::new_err(err.to_string())
}

fn call_python(
    py: Python<'_>,
    func: &Py<PyAny>,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
) -> PyResult<Value> {
    let mut py_args = Vec::with_capacity(args.len());
    for arg in args {
        py_args.push(value_to_py(py, arg)?);
    }
    let tuple = PyTuple::new(py, py_args)?;
    let result = if kwargs.is_empty() {
        func.call1(py, &tuple)?
    } else {
        let dict = PyDict::new(py);
        for (key, value) in kwargs {
            dict.set_item(key, value_to_py(py, value)?)?;
        }
        func.call(py, &tuple, Some(&dict))?
    };
    py_to_value(&result.into_bound(py))
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
fn weave(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyEngine>()?;
    m.add_class::<PySession>()?;
    m.add_class::<PyRun>()?;
    Ok(())
}
