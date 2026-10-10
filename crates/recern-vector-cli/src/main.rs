use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use recern_vector::{
    Collection, CollectionConfig, CollectionStats, Database, Filter, HnswParams, Metric,
    Quantization, RecallOptions, SearchOptions,
};
use serde_json::Value;

type CliResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(
    name = "recern-vector",
    version,
    about = "Recern Vector — a single-file vector database"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty database file
    Init { file: PathBuf },
    /// Create a collection
    CreateCollection {
        file: PathBuf,
        name: String,
        #[arg(long)]
        dim: usize,
        #[arg(long, default_value = "cosine")]
        metric: Metric,
        #[arg(long, default_value_t = 16)]
        m: usize,
        #[arg(long, default_value_t = 200)]
        ef_construction: usize,
        #[arg(long, default_value_t = 64)]
        ef_search: usize,
        #[arg(long, default_value = "f32", value_parser = ["f32", "int8"])]
        quantization: String,
    },
    /// Insert or replace records from JSON Lines: {"id": "...", "vector": [...], "metadata": {...}}
    Insert {
        file: PathBuf,
        collection: String,
        input: PathBuf,
        /// Threads for building the index (default: all cores)
        #[arg(long)]
        threads: Option<usize>,
    },
    /// Delete records by id
    Delete {
        file: PathBuf,
        collection: String,
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Find nearest neighbors of a vector or of a stored record
    Query {
        file: PathBuf,
        collection: String,
        /// Query vector as a JSON array
        #[arg(long, required_unless_present = "like", conflicts_with = "like")]
        vector: Option<String>,
        /// Use the vector of a stored record as the query
        #[arg(long)]
        like: Option<String>,
        #[arg(short, long, default_value_t = 10)]
        k: usize,
        #[arg(long)]
        ef: Option<usize>,
        /// Scan every record instead of using the index
        #[arg(long)]
        exact: bool,
        /// MongoDB-style metadata filter, e.g. '{"lang": "en", "year": {"$gte": 2020}}'
        #[arg(long)]
        filter: Option<String>,
        /// Show how the query was executed
        #[arg(long)]
        explain: bool,
    },
    /// Show collections, index structure and memory usage
    Inspect {
        file: PathBuf,
        collection: Option<String>,
    },
    /// Measure recall@k of the index against exact search
    Recall {
        file: PathBuf,
        collection: String,
        #[arg(long, default_value_t = 100)]
        sample: usize,
        #[arg(short, long, default_value_t = 10)]
        k: usize,
        #[arg(long, value_delimiter = ',', default_value = "16,32,64,128,256")]
        ef: Vec<usize>,
    },
    /// Merge WAL into a portable, standalone snapshot
    Checkpoint { file: PathBuf },
    /// Rebuild a collection without deleted records
    Compact { file: PathBuf, collection: String },
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> CliResult {
    match command {
        Command::Init { file } => {
            Database::create(&file)?;
            println!("created {}", file.display());
        }
        Command::CreateCollection {
            file,
            name,
            dim,
            metric,
            m,
            ef_construction,
            ef_search,
            quantization,
        } => {
            let mut db = Database::open(&file)?;
            let hnsw = HnswParams {
                m,
                ef_construction,
                ef_search,
            };
            db.create_collection(
                &name,
                CollectionConfig::new(dim, metric)
                    .with_hnsw(hnsw)
                    .with_quantization(if quantization == "int8" {
                        Quantization::Int8
                    } else {
                        Quantization::F32
                    }),
            )?;
            db.save()?;
            println!("created collection {name} ({dim} dims, {metric})");
        }
        Command::Insert {
            file,
            collection,
            input,
            threads,
        } => insert(file, &collection, input, threads)?,
        Command::Delete {
            file,
            collection,
            ids,
        } => {
            let mut db = Database::open(&file)?;
            let c = db.collection_mut(&collection)?;
            let deleted = ids.iter().filter(|id| c.delete(id)).count();
            db.save()?;
            println!("deleted {deleted} of {} records", ids.len());
        }
        Command::Query {
            file,
            collection,
            vector,
            like,
            k,
            ef,
            exact,
            filter,
            explain,
        } => {
            let db = Database::open_read_only(&file)?;
            let c = db.collection(&collection)?;
            let query = match (vector, like) {
                (Some(json), _) => serde_json::from_str::<Vec<f32>>(&json)
                    .map_err(|e| format!("--vector must be a JSON array of numbers: {e}"))?,
                (None, Some(id)) => c.get(&id).ok_or(format!("record not found: {id}"))?.vector,
                (None, None) => unreachable!("clap requires --vector or --like"),
            };
            let filter = match filter {
                Some(json) => {
                    let value = serde_json::from_str(&json)
                        .map_err(|e| format!("--filter must be a JSON object: {e}"))?;
                    Some(Filter::from_json(&value)?)
                }
                None => None,
            };
            let options = SearchOptions { ef, exact, filter };
            let report = c.explain(&query, k, &options)?;
            print_hits(&report.hits);
            if explain {
                println!();
                println!("  strategy      {}", report.strategy);
                if let Some(ef) = report.ef {
                    println!("  ef            {ef}");
                }
                if let Some(s) = report.filter_selectivity {
                    let precision = if s > 0.0 && s < 0.01 { 2 } else { 1 };
                    println!(
                        "  selectivity   ~{:.precision$}% of records match the filter",
                        s * 100.0
                    );
                }
                println!("  visited       {} nodes", count(report.visited));
                println!(
                    "  distances     {} computed",
                    count(report.distance_computations)
                );
                println!("  time          {}", duration(report.elapsed));
            }
        }
        Command::Inspect { file, collection } => {
            let db = Database::open_read_only(&file)?;
            let size = std::fs::metadata(&file)?.len();
            let names: Vec<&Collection> = match &collection {
                Some(name) => vec![db.collection(name)?],
                None => db.collections().collect(),
            };
            println!(
                "{} · format v{} · {} collection(s) · {} on disk",
                file.display(),
                db.format_version(),
                db.collections().count(),
                bytes(size as usize)
            );
            for c in names {
                println!();
                print_stats(&c.stats());
            }
        }
        Command::Recall {
            file,
            collection,
            sample,
            k,
            ef,
        } => {
            let db = Database::open_read_only(&file)?;
            let c = db.collection(&collection)?;
            let report = c.estimate_recall(&RecallOptions {
                sample,
                k,
                ef_values: ef,
                ..Default::default()
            })?;
            println!(
                "{collection} · recall@{} · {} sample queries · exact search p50 {}",
                report.k,
                report.sample,
                duration(report.exact_p50)
            );
            println!();
            println!(
                "  {:>6}  {:>8}  {:>10}  {:>10}",
                "ef", "recall", "p50", "p95"
            );
            for p in &report.points {
                let current = if p.ef == c.config().hnsw.ef_search {
                    "  ← ef_search"
                } else {
                    ""
                };
                println!(
                    "  {:>6}  {:>8.3}  {:>10}  {:>10}{current}",
                    p.ef,
                    p.recall,
                    duration(p.p50),
                    duration(p.p95)
                );
            }
        }
        Command::Checkpoint { file } => {
            Database::open(&file)?.checkpoint()?;
            println!("checkpointed {}", file.display());
        }
        Command::Compact { file, collection } => {
            let mut db = Database::open(&file)?;
            let removed = db.collection_mut(&collection)?.compact();
            db.save()?;
            println!("removed {removed} deleted records from {collection}");
        }
    }
    Ok(())
}

fn insert(file: PathBuf, collection: &str, input: PathBuf, threads: Option<usize>) -> CliResult {
    let mut db = Database::open(&file)?;
    let c = db.collection_mut(collection)?;
    let (dim, metric) = (c.config().dim, c.config().metric);
    let mut records = Vec::new();
    for (i, line) in BufReader::new(File::open(&input)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record = parse_record(&line)
            .and_then(|r| check_vector(&r.1, dim, metric).map(|()| r))
            .map_err(|e| format!("line {}: {e}", i + 1))?;
        records.push(record);
    }
    let threads =
        threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    let start = Instant::now();
    let inserted = c.upsert_many_with_threads(records, threads)?;
    let elapsed = start.elapsed();
    db.save()?;
    println!(
        "upserted {} records into {collection} in {}",
        count(inserted),
        duration(elapsed)
    );
    Ok(())
}

/// Mirrors the library's vector validation so errors can name the input line.
fn check_vector(vector: &[f32], dim: usize, metric: Metric) -> Result<(), String> {
    if vector.len() != dim {
        return Err(format!("expected {dim} dimensions, got {}", vector.len()));
    }
    if vector.iter().any(|x| !x.is_finite()) {
        return Err("vector contains NaN or infinite values".into());
    }
    if metric == Metric::Cosine && vector.iter().all(|&x| x == 0.0) {
        return Err("zero vector has no direction for cosine".into());
    }
    Ok(())
}

fn parse_record(line: &str) -> Result<(String, Vec<f32>, Option<Value>), String> {
    let mut value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let id = match value.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err("missing \"id\" (string or number)".into()),
    };
    let vector = value
        .get("vector")
        .and_then(Value::as_array)
        .ok_or("missing \"vector\" array")?
        .iter()
        .map(|x| {
            x.as_f64()
                .map(|x| x as f32)
                .ok_or("vector must contain only numbers")
        })
        .collect::<Result<Vec<f32>, _>>()?;
    let metadata = value
        .get_mut("metadata")
        .map(Value::take)
        .filter(|m| !m.is_null());
    Ok((id, vector, metadata))
}

fn print_hits(hits: &[recern_vector::SearchHit]) {
    if hits.is_empty() {
        println!("no results");
        return;
    }
    println!("  {:>4}  {:<24}  {:>10}  metadata", "#", "id", "distance");
    for (i, hit) in hits.iter().enumerate() {
        let metadata = hit
            .metadata
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_default();
        println!(
            "  {:>4}  {:<24}  {:>10.5}  {}",
            i + 1,
            truncate(&hit.id, 24),
            hit.distance,
            truncate(&metadata, 60)
        );
    }
}

fn print_stats(s: &CollectionStats) {
    println!("  encoding      {}", s.config.quantization.as_str());
    let total = s.live + s.deleted;
    println!("{}", s.name);
    println!(
        "  records       {} live · {} deleted",
        count(s.live),
        count(s.deleted)
    );
    println!(
        "  vectors       {} dims · {}",
        s.config.dim, s.config.metric
    );
    println!(
        "  index         HNSW m={} ef_construction={} ef_search={}",
        s.config.hnsw.m, s.config.hnsw.ef_construction, s.config.hnsw.ef_search
    );
    let layers: Vec<String> = s
        .nodes_per_layer
        .iter()
        .enumerate()
        .map(|(i, n)| format!("L{i} {}", count(*n)))
        .collect();
    if !layers.is_empty() {
        println!("  layers        {}", layers.join(" · "));
        println!(
            "  avg degree    {:.1} on L0 (max {})",
            s.avg_degree_layer0,
            s.config.hnsw.m * 2
        );
    }
    if s.unreachable == 0 {
        println!("  reachability  every live record is reachable");
    } else {
        println!(
            "  reachability  ⚠ {} live records unreachable from the entry point",
            count(s.unreachable)
        );
    }
    println!(
        "  memory        vectors {} · graph {} · metadata {}",
        bytes(s.vector_bytes),
        bytes(s.graph_bytes),
        bytes(s.metadata_bytes)
    );
    if total > 0 && s.deleted * 10 >= total {
        println!(
            "  hint          {:.0}% of nodes are deleted — run `recern-vector compact` to reclaim space",
            s.deleted as f64 * 100.0 / total as f64
        );
    }
}

fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn bytes(n: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn duration(d: Duration) -> String {
    let micros = d.as_secs_f64() * 1e6;
    if micros < 1000.0 {
        format!("{micros:.0} µs")
    } else if micros < 1e6 {
        format!("{:.2} ms", micros / 1e3)
    } else {
        format!("{:.2} s", micros / 1e6)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
