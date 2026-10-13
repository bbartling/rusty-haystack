//! One process-wide Tokio/Python bridge for async bindings. Client I/O runtimes
//! have independent ownership; closing a client never claims to destroy this.
use std::future::Future;
use std::pin::Pin;

use pyo3::prelude::*;
use pyo3_async_runtimes::{TaskLocals, generic};

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    pyo3_async_runtimes::tokio::get_runtime()
}

// The pinned library's default Tokio adapter unconditionally calls
// Python::attach in its late result-notification job. A client destructor can
// finish native work during interpreter finalization. Use the public generic
// adapter boundary to skip that notification when Python is no longer usable.
// The successful try_attach guard remains held through the nested library
// callback, rather than testing interpreter availability and then racing it.
struct BridgeRuntime;
impl generic::Runtime for BridgeRuntime {
    type JoinError = tokio::task::JoinError;
    type JoinHandle = tokio::task::JoinHandle<()>;

    fn spawn<F>(future: F) -> Self::JoinHandle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        runtime().spawn(future)
    }

    fn spawn_blocking<F>(callback: F) -> Self::JoinHandle
    where
        F: FnOnce() + Send + 'static,
    {
        runtime().spawn_blocking(move || {
            let _ = Python::try_attach(|_| callback());
        })
    }
}

tokio::task_local! { static LOCALS: TaskLocals; }
impl generic::ContextExt for BridgeRuntime {
    fn scope<F, R>(locals: TaskLocals, future: F) -> Pin<Box<dyn Future<Output = R> + Send>>
    where
        F: Future<Output = R> + Send + 'static,
    {
        Box::pin(LOCALS.scope(locals, future))
    }
    fn get_task_locals() -> Option<TaskLocals> {
        LOCALS.try_with(Clone::clone).ok()
    }
}

pub(crate) fn future<'py, F>(
    py: Python<'py>,
    locals: TaskLocals,
    future: F,
) -> PyResult<Bound<'py, PyAny>>
where
    F: Future<Output = PyResult<Py<PyAny>>> + Send + 'static,
{
    // Preserve the public Tokio helper context too, so later async bindings
    // can compose explicit generic and ordinary Tokio conversions. Scope here:
    // ContextExt::scope itself cannot add a static bound to its generic output.
    let future = pyo3_async_runtimes::tokio::scope(locals.clone(), future);
    generic::future_into_py_with_locals::<BridgeRuntime, _, _>(py, locals, future)
}

pub(crate) fn with_python<T>(
    callback: impl for<'py> FnOnce(Python<'py>) -> PyResult<T>,
) -> PyResult<T> {
    Python::try_attach(callback).unwrap_or_else(|| {
        Err(pyo3::exceptions::PyRuntimeError::new_err(
            "Python interpreter is unavailable",
        ))
    })
}
