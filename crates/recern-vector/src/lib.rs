//! Recern Vector — a single-file, embedded, inspectable vector database.
//!
//! A snapshot and a WAL hold any number of collections. Each collection
//! stores vectors of a fixed dimension, optional JSON metadata, and an HNSW
//! index whose internals can be inspected through [`Collection::stats`],
//! [`Collection::explain`] and [`Collection::estimate_recall`].
//!
//! ```no_run
//! use recern_vector::{CollectionConfig, Database, Metric, SearchOptions};
//!
//! let mut db = Database::open_or_create("docs.rvec")?;
//! let docs = db.create_collection("docs", CollectionConfig::new(3, Metric::Cosine))?;
//! docs.upsert("a", &[0.1, 0.9, 0.0], None)?;
//! docs.upsert("b", &[0.8, 0.1, 0.1], None)?;
//! let hits = docs.search(&[0.2, 0.8, 0.0], 1, &SearchOptions::default())?;
//! assert_eq!(hits[0].id, "a");
//! db.save()?;
//! # Ok::<(), recern_vector::Error>(())
//! ```

mod collection;
mod database;
mod error;
mod filter;
mod hnsw;
mod metric;
mod rng;
mod storage;
mod wal;

pub use collection::{
    Collection, CollectionConfig, CollectionStats, Quantization, RecallOptions, RecallPoint,
    RecallReport, Record, SearchHit, SearchOptions, SearchReport, Strategy,
};
pub use database::Database;
pub use error::{Error, Result};
pub use filter::Filter;
pub use hnsw::HnswParams;
pub use metric::Metric;
pub use storage::FORMAT_VERSION;
