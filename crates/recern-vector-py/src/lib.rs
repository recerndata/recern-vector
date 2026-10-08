//! Python bindings for Recern Vector, exposed as `recern_vector._native` and
//! re-exported from the `recern_vector` package.

use std::path::PathBuf;
use std::sync::{PoisonError, RwLock};

use pyo3::buffer::PyBuffer;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyFileExistsError, PyKeyError, PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use recern_vector as rv;
use serde_json::Value;

// Most errors map to built-in exceptions (ValueError, KeyError, OSError,
// FileExistsError); only database-file problems get their own type.
create_exception!(
    recern_vector,
    RecernVectorError,
    PyException,
    "Base class for Recern Vector errors."
);
create_exception!(
    recern_vector,
    CorruptDatabaseError,
    RecernVectorError,
    "The file is not a valid Recern Vector database, or was written by a newer version."
);

fn to_py_err(err: rv::Error) -> PyErr {
    let message = err.to_string();
    match err {
        rv::Error::Io(_) => PyOSError::new_err(message),
        rv::Error::Corrupt(_) | rv::Error::UnsupportedVersion(_) => {
            CorruptDatabaseError::new_err(message)
        }
        rv::Error::FileExists(_) => PyFileExistsError::new_err(message),
        rv::Error::CollectionNotFound(_) => PyKeyError::new_err(message),
        rv::Error::CollectionExists(_)
        | rv::Error::DimensionMismatch { .. }
        | rv::Error::InvalidVector(_)
        | rv::Error::InvalidArgument(_) => PyValueError::new_err(message),
    }
}

trait IntoPyResult<T> {
    fn py_err(self) -> PyResult<T>;
}

impl<T> IntoPyResult<T> for rv::Result<T> {
    fn py_err(self) -> PyResult<T> {
        self.map_err(to_py_err)
    }
}

// ---------------------------------------------------------------- conversions

/// Reads a 1-D vector. float32 buffers (e.g. `np.float32` arrays) are read
/// directly; any other sequence of numbers is converted element by element.
fn extract_vector(obj: &Bound<'_, PyAny>) -> PyResult<Vec<f32>> {
    if let Ok(buffer) = PyBuffer::<f32>::get(obj) {
        if buffer.dimensions() != 1 {
            return Err(PyValueError::new_err("expected a 1-D vector"));
        }
        return buffer.to_vec(obj.py());
    }
    obj.extract::<Vec<f32>>()
        .map_err(|_| PyValueError::new_err("expected a sequence of numbers or a float32 array"))
}

/// Reads a batch of vectors: a C-contiguous 2-D float32 buffer, or a
/// sequence of 1-D vectors.
fn extract_matrix(obj: &Bound<'_, PyAny>) -> PyResult<(Vec<f32>, usize, usize)> {
    if let Ok(buffer) = PyBuffer::<f32>::get(obj) {
        if buffer.dimensions() != 2 {
            return Err(PyValueError::new_err("expected a 2-D array of vectors"));
        }
        let (rows, cols) = (buffer.shape()[0], buffer.shape()[1]);
        return Ok((buffer.to_vec(obj.py())?, rows, cols));
    }
    let mut data = Vec::new();
    let mut rows = 0;
    let mut cols = None;
    for item in obj.try_iter()? {
        let vector = extract_vector(&item?)?;
        if *cols.get_or_insert(vector.len()) != vector.len() {
            return Err(PyValueError::new_err(
                "all vectors must have the same length",
            ));
        }
        data.extend_from_slice(&vector);
        rows += 1;
    }
    Ok((data, rows, cols.unwrap_or(0)))
}

fn to_json(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if obj.is_none() {
        Ok(Value::Null)
    } else if let Ok(b) = obj.cast::<PyBool>() {
        Ok(Value::Bool(b.is_true()))
    } else if obj.is_instance_of::<PyInt>() {
        if let Ok(i) = obj.extract::<i64>() {
            Ok(Value::from(i))
        } else {
            Ok(Value::from(obj.extract::<u64>().map_err(|_| {
                PyValueError::new_err("integer metadata values must fit in 64 bits")
            })?))
        }
    } else if let Ok(f) = obj.cast::<PyFloat>() {
        serde_json::Number::from_f64(f.value())
            .map(Value::Number)
            .ok_or_else(|| PyValueError::new_err("metadata cannot contain NaN or infinity"))
    } else if let Ok(s) = obj.cast::<PyString>() {
        Ok(Value::String(s.to_str()?.to_owned()))
    } else if let Ok(dict) = obj.cast::<PyDict>() {
        let mut map = serde_json::Map::with_capacity(dict.len());
        for (key, value) in dict.iter() {
            let key = key
                .cast::<PyString>()
                .map_err(|_| PyValueError::new_err("metadata keys must be strings"))?;
            map.insert(key.to_str()?.to_owned(), to_json(&value)?);
        }
        Ok(Value::Object(map))
    } else if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
        obj.try_iter()?
            .map(|item| to_json(&item?))
            .collect::<PyResult<Vec<_>>>()
            .map(Value::Array)
    } else {
        Err(PyValueError::new_err(format!(
            "unsupported metadata type: {}",
            obj.get_type().name()?
        )))
    }
}

fn from_json<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match value {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.into_pyobject(py)?.into_any(),
            (None, Some(u)) => u.into_pyobject(py)?.into_any(),
            _ => n.as_f64().unwrap_or(f64::NAN).into_pyobject(py)?.into_any(),
        },
        Value::String(s) => PyString::new(py, s).into_any(),
        Value::Array(items) => {
            let items = items
                .iter()
                .map(|v| from_json(py, v))
                .collect::<PyResult<Vec<_>>>()?;
            PyList::new(py, items)?.into_any()
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (key, value) in map {
                dict.set_item(key, from_json(py, value)?)?;
            }
            dict.into_any()
        }
    })
}

fn optional_json(py: Python<'_>, value: &Option<Value>) -> PyResult<Py<PyAny>> {
    match value {
        Some(value) => Ok(from_json(py, value)?.unbind()),
        None => Ok(py.None()),
    }
}

fn extract_metadata(obj: Option<&Bound<'_, PyAny>>) -> PyResult<Option<Value>> {
    match obj {
        Some(obj) if !obj.is_none() => Ok(Some(to_json(obj)?)),
        _ => Ok(None),
    }
}

fn extract_filter(obj: Option<&Bound<'_, PyAny>>) -> PyResult<Option<rv::Filter>> {
    match obj {
        Some(obj) if !obj.is_none() => Ok(Some(rv::Filter::from_json(&to_json(obj)?).py_err()?)),
        _ => Ok(None),
    }
}

// --------------------------------------------------------------- result types

/// One search result.
#[pyclass(module = "recern_vector", frozen, get_all, skip_from_py_object)]
struct Hit {
    id: String,
    distance: f32,
    metadata: Py<PyAny>,
}

#[pymethods]
impl Hit {
    fn __repr__(&self) -> String {
        format!("Hit(id={:?}, distance={:.6})", self.id, self.distance)
    }
}

fn hits(py: Python<'_>, hits: Vec<rv::SearchHit>) -> PyResult<Vec<Hit>> {
    hits.into_iter()
        .map(|h| {
            Ok(Hit {
                id: h.id,
                distance: h.distance,
                metadata: optional_json(py, &h.metadata)?,
            })
        })
        .collect()
}

/// A stored record. For cosine collections `vector` is normalized.
#[pyclass(module = "recern_vector", frozen, get_all, skip_from_py_object)]
struct Record {
    id: String,
    vector: Vec<f32>,
    metadata: Py<PyAny>,
}

#[pymethods]
impl Record {
    fn __repr__(&self) -> String {
        format!("Record(id={:?}, dim={})", self.id, self.vector.len())
    }
}

/// Search results together with how the query was executed.
#[pyclass(module = "recern_vector", frozen, get_all, skip_from_py_object)]
struct SearchReport {
    hits: Vec<Py<Hit>>,
    /// `"hnsw"`, `"exact"` or `"filtered_exact"`.
    strategy: &'static str,
    ef: Option<usize>,
    visited: usize,
    distance_computations: usize,
    filter_selectivity: Option<f64>,
    elapsed_ms: f64,
}

#[pymethods]
impl SearchReport {
    fn __repr__(&self) -> String {
        format!(
            "SearchReport(strategy={:?}, hits={}, visited={}, elapsed_ms={:.3})",
            self.strategy,
            self.hits.len(),
            self.visited,
            self.elapsed_ms
        )
    }
}

#[pyclass(module = "recern_vector", frozen, get_all, skip_from_py_object)]
struct RecallPoint {
    ef: usize,
    recall: f64,
    p50_ms: f64,
    p95_ms: f64,
}

#[pymethods]
impl RecallPoint {
    fn __repr__(&self) -> String {
        format!(
            "RecallPoint(ef={}, recall={:.3}, p50_ms={:.3})",
            self.ef, self.recall, self.p50_ms
        )
    }
}

#[pyclass(module = "recern_vector", frozen, get_all, skip_from_py_object)]
struct RecallReport {
    k: usize,
    sample: usize,
    exact_p50_ms: f64,
    points: Vec<Py<RecallPoint>>,
}

#[pymethods]
impl RecallReport {
    fn __repr__(&self) -> String {
        format!(
            "RecallReport(k={}, sample={}, points={})",
            self.k,
            self.sample,
            self.points.len()
        )
    }
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

// ------------------------------------------------------------------- database

/// A database file and its collections.
///
/// Everything is held in memory; call `save()` (or use the database as a
/// context manager) to write changes to disk atomically.
///
/// Reads (searches, `get`, `stats`, `save`) share a lock and run in parallel
/// from several threads; writes wait for them and run alone. The lock is
/// always taken with the GIL released, and no Python code runs while it is
/// held, so threads cannot deadlock on it.
#[pyclass(module = "recern_vector", frozen)]
struct Database {
    inner: RwLock<rv::Database>,
}

impl Database {
    fn wrap(inner: rv::Database) -> Self {
        Self {
            inner: RwLock::new(inner),
        }
    }

    fn read<R: Send>(&self, py: Python<'_>, f: impl FnOnce(&rv::Database) -> R + Send) -> R {
        py.detach(|| f(&self.inner.read().unwrap_or_else(PoisonError::into_inner)))
    }

    fn write<R: Send>(&self, py: Python<'_>, f: impl FnOnce(&mut rv::Database) -> R + Send) -> R {
        py.detach(|| f(&mut self.inner.write().unwrap_or_else(PoisonError::into_inner)))
    }
}

#[pymethods]
impl Database {
    /// Opens an existing database file.
    #[new]
    fn new(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| rv::Database::open(path)).py_err()?;
        Ok(Self::wrap(inner))
    }

    /// Creates a new, empty database file. Fails if the file exists.
    #[staticmethod]
    fn create(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| rv::Database::create(path)).py_err()?;
        Ok(Self::wrap(inner))
    }

    /// Opens the database file, creating it if it does not exist.
    #[staticmethod]
    fn open_or_create(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| rv::Database::open_or_create(path)).py_err()?;
        Ok(Self::wrap(inner))
    }

    #[getter]
    fn path(&self, py: Python<'_>) -> PathBuf {
        self.read(py, |db| db.path().to_path_buf())
    }

    #[pyo3(signature = (name, dim, metric = "cosine", m = 16, ef_construction = 200, ef_search = 64))]
    fn create_collection(
        slf: Bound<'_, Self>,
        name: &str,
        dim: usize,
        metric: &str,
        m: usize,
        ef_construction: usize,
        ef_search: usize,
    ) -> PyResult<Collection> {
        let metric: rv::Metric = metric.parse().map_err(PyValueError::new_err)?;
        let hnsw = rv::HnswParams {
            m,
            ef_construction,
            ef_search,
        };
        let config = rv::CollectionConfig::new(dim, metric).with_hnsw(hnsw);
        slf.get()
            .write(slf.py(), |db| {
                db.create_collection(name, config).map(|_| ())
            })
            .py_err()?;
        Ok(Collection {
            db: slf.unbind(),
            name: name.to_owned(),
        })
    }

    fn collection(slf: Bound<'_, Self>, name: &str) -> PyResult<Collection> {
        slf.get()
            .read(slf.py(), |db| db.collection(name).map(|_| ()))
            .py_err()?;
        Ok(Collection {
            db: slf.unbind(),
            name: name.to_owned(),
        })
    }

    fn drop_collection(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        self.write(py, |db| db.drop_collection(name)).py_err()
    }

    fn collection_names(&self, py: Python<'_>) -> Vec<String> {
        self.read(py, |db| {
            db.collections().map(|c| c.name().to_owned()).collect()
        })
    }

    /// Writes the database to disk atomically.
    fn save(&self, py: Python<'_>) -> PyResult<()> {
        self.read(py, |db| db.save()).py_err()
    }

    fn __getitem__(slf: Bound<'_, Self>, name: &str) -> PyResult<Collection> {
        Self::collection(slf, name)
    }

    fn __contains__(&self, py: Python<'_>, name: &str) -> bool {
        self.read(py, |db| db.collection(name).is_ok())
    }

    fn __enter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    /// Saves on a clean exit; discards nothing on error, but does not save.
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: Option<&Bound<'_, PyAny>>,
        _exc_value: Option<&Bound<'_, PyAny>>,
        _traceback: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        if exc_type.is_none_or(|t| t.is_none()) {
            self.save(py)?;
        }
        Ok(false)
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        format!(
            "Database(path={:?}, collections={:?})",
            self.path(py).display().to_string(),
            self.collection_names(py)
        )
    }
}

// ----------------------------------------------------------------- collection

/// A handle to a collection inside a `Database`.
#[pyclass(module = "recern_vector", frozen)]
struct Collection {
    db: Py<Database>,
    name: String,
}

impl Collection {
    fn with<R: Send>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&rv::Collection) -> rv::Result<R> + Send,
    ) -> PyResult<R> {
        let name = &self.name;
        self.db
            .get()
            .read(py, |db| db.collection(name).and_then(f))
            .py_err()
    }

    fn with_mut<R: Send>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&mut rv::Collection) -> rv::Result<R> + Send,
    ) -> PyResult<R> {
        let name = &self.name;
        self.db
            .get()
            .write(py, |db| db.collection_mut(name).and_then(f))
            .py_err()
    }
}

#[pymethods]
impl Collection {
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn dim(&self, py: Python<'_>) -> PyResult<usize> {
        self.with(py, |c| Ok(c.config().dim))
    }

    #[getter]
    fn metric(&self, py: Python<'_>) -> PyResult<&'static str> {
        self.with(py, |c| Ok(c.config().metric.as_str()))
    }

    /// Inserts a record, replacing any record with the same id.
    #[pyo3(signature = (id, vector, metadata = None))]
    fn upsert(
        &self,
        py: Python<'_>,
        id: &str,
        vector: &Bound<'_, PyAny>,
        metadata: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let vector = extract_vector(vector)?;
        let metadata = extract_metadata(metadata)?;
        self.with_mut(py, |c| c.upsert(id, &vector, metadata))
    }

    /// Inserts many records and builds their index links in parallel.
    /// `vectors` may be a 2-D float32 array (fastest) or a sequence of
    /// vectors. The batch is atomic. `threads` defaults to all cores; with 1
    /// the result is identical to upserting records one by one. Returns the
    /// number of records written.
    #[pyo3(signature = (ids, vectors, metadatas = None, *, threads = None))]
    fn upsert_many(
        &self,
        py: Python<'_>,
        ids: Vec<String>,
        vectors: &Bound<'_, PyAny>,
        metadatas: Option<&Bound<'_, PyAny>>,
        threads: Option<usize>,
    ) -> PyResult<usize> {
        let (data, rows, cols) = extract_matrix(vectors)?;
        if rows != ids.len() {
            return Err(PyValueError::new_err(format!(
                "got {} ids but {rows} vectors",
                ids.len()
            )));
        }
        let metadatas: Vec<Option<Value>> = match metadatas {
            Some(m) if !m.is_none() => m
                .try_iter()?
                .map(|item| extract_metadata(Some(&item?)))
                .collect::<PyResult<_>>()?,
            _ => vec![None; rows],
        };
        if metadatas.len() != rows {
            return Err(PyValueError::new_err(format!(
                "got {} metadata entries but {rows} vectors",
                metadatas.len()
            )));
        }
        let threads =
            threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
        self.with_mut(py, |collection| {
            let records = ids
                .iter()
                .enumerate()
                .zip(metadatas)
                .map(|((row, id), metadata)| (id, &data[row * cols..(row + 1) * cols], metadata));
            collection.upsert_many_with_threads(records, threads)
        })
    }

    /// Removes a record. Returns whether it existed.
    fn delete(&self, py: Python<'_>, id: &str) -> PyResult<bool> {
        self.with_mut(py, |c| Ok(c.delete(id)))
    }

    fn get(&self, py: Python<'_>, id: &str) -> PyResult<Option<Record>> {
        self.with(py, |c| Ok(c.get(id)))?
            .map(|r| {
                Ok(Record {
                    id: r.id,
                    vector: r.vector,
                    metadata: optional_json(py, &r.metadata)?,
                })
            })
            .transpose()
    }

    /// Returns the `k` nearest records.
    #[pyo3(signature = (query, k = 10, *, ef = None, exact = false, filter = None))]
    fn search(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyAny>,
        k: usize,
        ef: Option<usize>,
        exact: bool,
        filter: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<Hit>> {
        let report = self.run_search(py, query, k, ef, exact, filter)?;
        hits(py, report.hits)
    }

    /// Like `search`, but also reports the strategy and work done.
    #[pyo3(signature = (query, k = 10, *, ef = None, exact = false, filter = None))]
    fn explain(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyAny>,
        k: usize,
        ef: Option<usize>,
        exact: bool,
        filter: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<SearchReport> {
        let report = self.run_search(py, query, k, ef, exact, filter)?;
        Ok(SearchReport {
            hits: hits(py, report.hits)?
                .into_iter()
                .map(|h| Py::new(py, h))
                .collect::<PyResult<_>>()?,
            strategy: match report.strategy {
                rv::Strategy::Hnsw => "hnsw",
                rv::Strategy::Exact => "exact",
                rv::Strategy::FilteredExact => "filtered_exact",
            },
            ef: report.ef,
            visited: report.visited,
            distance_computations: report.distance_computations,
            filter_selectivity: report.filter_selectivity,
            elapsed_ms: ms(report.elapsed),
        })
    }

    /// Index structure, reachability and memory usage.
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let s = self.with(py, |c| Ok(c.stats()))?;
        let dict = PyDict::new(py);
        dict.set_item("name", s.name)?;
        dict.set_item("dim", s.config.dim)?;
        dict.set_item("metric", s.config.metric.as_str())?;
        dict.set_item("m", s.config.hnsw.m)?;
        dict.set_item("ef_construction", s.config.hnsw.ef_construction)?;
        dict.set_item("ef_search", s.config.hnsw.ef_search)?;
        dict.set_item("live", s.live)?;
        dict.set_item("deleted", s.deleted)?;
        dict.set_item("nodes_per_layer", s.nodes_per_layer)?;
        dict.set_item("avg_degree_layer0", s.avg_degree_layer0)?;
        dict.set_item("unreachable", s.unreachable)?;
        dict.set_item("vector_bytes", s.vector_bytes)?;
        dict.set_item("graph_bytes", s.graph_bytes)?;
        dict.set_item("metadata_bytes", s.metadata_bytes)?;
        Ok(dict)
    }

    /// Measures recall@k of the index against exact search.
    #[pyo3(signature = (sample = 100, k = 10, ef_values = vec![16, 32, 64, 128, 256], seed = 42))]
    fn estimate_recall(
        &self,
        py: Python<'_>,
        sample: usize,
        k: usize,
        ef_values: Vec<usize>,
        seed: u64,
    ) -> PyResult<RecallReport> {
        let options = rv::RecallOptions {
            sample,
            k,
            ef_values,
            seed,
        };
        let report = self.with(py, |c| c.estimate_recall(&options))?;
        Ok(RecallReport {
            k: report.k,
            sample: report.sample,
            exact_p50_ms: ms(report.exact_p50),
            points: report
                .points
                .into_iter()
                .map(|p| {
                    Py::new(
                        py,
                        RecallPoint {
                            ef: p.ef,
                            recall: p.recall,
                            p50_ms: ms(p.p50),
                            p95_ms: ms(p.p95),
                        },
                    )
                })
                .collect::<PyResult<_>>()?,
        })
    }

    /// Rebuilds the collection without deleted records. Returns how many
    /// were removed.
    fn compact(&self, py: Python<'_>) -> PyResult<usize> {
        self.with_mut(py, |c| Ok(c.compact()))
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        self.with(py, |c| Ok(c.len()))
    }

    fn __contains__(&self, py: Python<'_>, id: &str) -> PyResult<bool> {
        self.with(py, |c| Ok(c.contains(id)))
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let (dim, metric, len) = self.with(py, |c| {
            Ok((c.config().dim, c.config().metric.as_str(), c.len()))
        })?;
        Ok(format!(
            "Collection(name={:?}, dim={dim}, metric={metric:?}, len={len})",
            self.name
        ))
    }
}

impl Collection {
    fn run_search(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyAny>,
        k: usize,
        ef: Option<usize>,
        exact: bool,
        filter: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<rv::SearchReport> {
        let query = extract_vector(query)?;
        let options = rv::SearchOptions {
            ef,
            exact,
            filter: extract_filter(filter)?,
        };
        self.with(py, |c| c.explain(&query, k, &options))
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Database>()?;
    m.add_class::<Collection>()?;
    m.add_class::<Hit>()?;
    m.add_class::<Record>()?;
    m.add_class::<SearchReport>()?;
    m.add_class::<RecallPoint>()?;
    m.add_class::<RecallReport>()?;
    m.add("RecernVectorError", m.py().get_type::<RecernVectorError>())?;
    m.add(
        "CorruptDatabaseError",
        m.py().get_type::<CorruptDatabaseError>(),
    )?;
    m.add("FORMAT_VERSION", rv::FORMAT_VERSION)?;
    Ok(())
}
