"""Isolated-process, release-build persistence and ANN measurements.

Historical 0.4.0 harness/results are left untouched. See the accompanying
bench/releases/0.5.0-dev.1/README.md for commands, scope and memory definitions.
Only newly created work directories are used; existing reports are not replaced.
"""
from __future__ import annotations
import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import statistics
import struct
import subprocess
import sys
import tempfile
import time
import uuid
import zlib
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
DATASETS = {"sift": ("sift-128-euclidean.hdf5", "l2"), "glove": ("glove-100-angular.hdf5", "cosine")}


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def command(args):
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout.strip()


def peak_rss():
    if sys.platform not in ("darwin", "linux"):
        return None
    import resource
    value = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return int(value if sys.platform == "darwin" else value * 1024)


def package():
    import recern_vector as rv
    native = __import__("recern_vector._native", fromlist=["_native"])
    return {"version": importlib.metadata.version("recern-vector"),
            "native_sha256": sha(native.__file__), "python": platform.python_version(),
            "numpy": importlib.metadata.version("numpy"), "native_module": Path(native.__file__).name}


def distribution(values):
    import numpy as np
    return {"p50_ms": float(np.percentile(values, 50)), "p95_ms": float(np.percentile(values, 95)),
            "min_ms": min(values), "max_ms": max(values), "samples_ms": values}


def timed(call):
    start = time.perf_counter_ns()
    result = call()
    return result, (time.perf_counter_ns() - start) / 1e6


def initialize(rv, path, layout):
    if layout == 3:
        return rv.Database.create(path)
    # Documented empty format 2, with a new identity and no WAL. This lets the
    # current engine's legacy path be measured without silently converting it.
    body = b"RVEC" + struct.pack("<I", 2) + uuid.uuid4().bytes + struct.pack("<QI", 0, 0)
    with path.open("xb") as stream:
        stream.write(body + struct.pack("<I", zlib.crc32(body)))
    return rv.Database(path)


def persistence(case, folder):
    import numpy as np
    import recern_vector as rv
    path = folder / "bench.rvec"
    train = np.random.default_rng(20261104).standard_normal((case["records"], case["dim"]), dtype=np.float32)
    db = initialize(rv, path, case["format"])
    if hasattr(db, "set_checkpoint_policy"):
        db.set_checkpoint_policy()  # compare foreground save cost with maintenance disabled
    col = db.create_collection("bench", case["dim"], metric="cosine", quantization=case["precision"])
    _, build = timed(lambda: col.upsert_many([str(i) for i in range(len(train))], train, threads=case["threads"]))
    peaks = {"after_build": peak_rss()}
    _, first_save = timed(db.save)
    peaks["after_first_save"] = peak_rss()
    _, first_checkpoint = timed(db.checkpoint)
    peaks["after_first_checkpoint"] = peak_rss()
    base_bytes = path.stat().st_size
    phases = {}
    for phase in ["update", "delete", "no_op"]:
        latencies, mutations, growth, counters = [], [], [], []
        for i in range(case["commits"]):
            if phase == "update":
                _, ms = timed(lambda: col.upsert(str(case["commits"] + i), train[i], {"revision": i}))
            elif phase == "delete":
                found, ms = timed(lambda: col.delete(str(i)))
                assert found
            else:
                ms = 0.0
            mutations.append(ms)
            wal = Path(str(path) + ".wal")
            before = wal.stat().st_size
            _, ms = timed(db.save)
            latencies.append(ms)
            growth.append(wal.stat().st_size - before)
            counters.append(db.persistence_stats() if hasattr(db, "persistence_stats") else None)
        phases[phase] = {**distribution(latencies), "mutation_ms": mutations,
                         "wal_growth_bytes": growth, "save_work": counters}
        peaks["after_" + phase] = peak_rss()
    expected = len(train) - case["commits"]
    stats = col.stats()
    _, checkpoint = timed(db.checkpoint)
    peaks["after_final_checkpoint"] = peak_rss()
    del col, db, train
    import gc
    gc.collect()
    reopened, opened = timed(lambda: rv.Database.open_read_only(path))
    assert len(reopened["bench"]) == expected
    peaks["after_reopen"] = peak_rss()
    return {"build_ms": build, "first_save_ms": first_save, "first_checkpoint_ms": first_checkpoint,
            "final_checkpoint_ms": checkpoint, "warm_open_ms": opened,
            "snapshot_bytes_before_mutations": base_bytes, "snapshot_bytes_after_mutations": path.stat().st_size,
            "collection_stats": stats, "phases": phases, "peak_rss_bytes": peak_rss(),
            "cumulative_peak_rss_bytes_by_stage": peaks}


def ann(case, folder):
    import gc
    import h5py
    import numpy as np
    import recern_vector as rv
    filename, metric = DATASETS[case["dataset"]]
    with h5py.File(Path(case["data_dir"]) / filename) as data:
        train = np.asarray(data["train"][:case["subset"]] if case["subset"] else data["train"], dtype=np.float32)
        queries = np.asarray(data["test"][:case["queries"]], dtype=np.float32)
        truth = np.asarray(data["neighbors"][:case["queries"], :10])
    assert len(queries) == case["queries"]
    if case["subset"]:
        # Smoke runs must recompute ground truth for their smaller training set.
        x = train.copy()
        if metric == "cosine":
            x /= np.linalg.norm(x, axis=1, keepdims=True)
        truth = np.asarray([np.argsort(-(x @ q))[:10] if metric == "cosine"
                            else np.argsort(np.sum((x-q)**2, axis=1))[:10] for q in queries])
        del x
    path = folder / "ann.rvec"
    db = rv.Database.create(path)
    db.set_checkpoint_policy()
    col = db.create_collection("bench", train.shape[1], metric=metric, m=16, ef_construction=200, quantization=case["precision"])
    _, build = timed(lambda: col.upsert_many([str(i) for i in range(len(train))], train, threads=case["threads"]))
    stats = col.stats()
    print(f"ANN {case['dataset']} {case['precision']}: built {len(train)} records in {build/1000:.2f}s", file=sys.stderr, flush=True)
    peaks = {"after_build": peak_rss()}
    _, saved = timed(db.save)
    peaks["after_save"] = peak_rss()
    _, checkpoint = timed(db.checkpoint)
    peaks["after_checkpoint"] = peak_rss()
    del col, db, train
    gc.collect()
    db, opened = timed(lambda: rv.Database.open_read_only(path))
    peaks["after_open"] = peak_rss()
    col = db["bench"]
    print(f"ANN {case['dataset']} {case['precision']}: save {saved/1000:.2f}s, checkpoint {checkpoint/1000:.2f}s, open {opened/1000:.2f}s", file=sys.stderr, flush=True)
    runs = []
    for ef in [10, 20, 40, 80, 160, 320, 640, 1280]:
        repetitions = []
        for repeat in range(case["repeats"]):
            for q in queries[:100]:
                col.search(q, k=10, ef=ef)
            times, recalls = [], []
            for i in np.random.default_rng(20261102 + repeat).permutation(len(queries)):
                start = time.perf_counter_ns()
                found = [int(h.id) for h in col.search(queries[i], k=10, ef=ef)]
                times.append((time.perf_counter_ns()-start)/1e6)
                recalls.append(len(set(found) & set(truth[i].tolist()))/10)
            repetitions.append({"recall": float(np.mean(recalls)), "p50_ms": float(np.percentile(times,50)),
                                "p95_ms": float(np.percentile(times,95)), "qps": len(times)*1000/sum(times)})
        runs.append({"ef": ef, **{key: statistics.median(r[key] for r in repetitions) for key in repetitions[0]},
                     "qps_min": min(r["qps"] for r in repetitions), "qps_max": max(r["qps"] for r in repetitions),
                     "repetitions": repetitions})
        print(f"ANN ef={ef}: recall={runs[-1]['recall']:.4f}, qps={runs[-1]['qps']:.0f}", file=sys.stderr, flush=True)
    return {"records": stats["live"], "dim": stats["dim"], "metric": metric, "format": db.format_version,
            "build_ms": build, "save_ms": saved, "checkpoint_ms": checkpoint, "warm_open_ms": opened,
            "snapshot_bytes": path.stat().st_size, "collection_stats": stats, "runs": runs,
            "peak_rss_bytes": peak_rss(), "cumulative_peak_rss_bytes_by_stage": peaks}


def worker():
    case = json.load(sys.stdin)
    assert importlib.metadata.version("recern-vector") == case["version"]
    with tempfile.TemporaryDirectory(prefix="case-", dir=case["work"]) as directory:
        result = (persistence if case["mode"] == "persistence" else ann)(case, Path(directory))
        print(json.dumps({"case": {k:v for k,v in case.items() if k not in ("work","data_dir")}, "package": package(), **result}, allow_nan=False))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("mode", choices=["persistence", "ann"])
    p.add_argument("--current-python", type=Path, required=True)
    p.add_argument("--current-version")
    p.add_argument("--baseline-python", type=Path)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--work", type=Path, required=True)
    p.add_argument("--sizes", default="1000,10000,100000")
    p.add_argument("--dim", type=int, default=128)
    p.add_argument("--commits", type=int, default=20)
    p.add_argument("--repeats", type=int, default=3)
    p.add_argument("--threads", type=int, default=4)
    p.add_argument("--dataset", choices=DATASETS, default="sift")
    p.add_argument("--data-dir", type=Path, default=ROOT/"bench-data")
    p.add_argument("--queries", type=int, default=1000)
    p.add_argument("--subset", type=int)
    a = p.parse_args()
    if a.current_version is None:
        sys.path.insert(0, str(ROOT/"scripts"))
        from release_version import python_version
        a.current_version = python_version()
    if min(a.dim,a.commits,a.repeats,a.threads,a.queries) < 1 or (a.subset is not None and a.subset < 10):
        p.error("all limits must be positive; subset must contain at least 10 records")
    sizes = [int(n) for n in a.sizes.split(",")]
    if min(sizes) <= a.commits * 2:
        p.error("each size must exceed twice the commit count")
    if a.output.exists(): raise FileExistsError(a.output)
    a.work = a.work.resolve(); a.work.mkdir(parents=True, exist_ok=False)
    report = {"schema":1, "scope":"smoke subset" if a.subset else "full ANN dataset" if a.mode=="ann" else "synthetic persistence",
              "date":datetime.now(timezone.utc).isoformat(), "mode":a.mode,
              "machine":{"platform":platform.platform(),"architecture":platform.machine()},
              "source":{"base_commit":command(["git","rev-parse","HEAD"]),"harness_sha256":sha(__file__)},
              "method":{"profile":"release", "threads":a.threads,"precision_modes":["f32","int8"],
                        "memory":"process lifetime high-water RSS, including inputs, bindings, graph, encoded pages and recovery buffers; stage values are cumulative, not phase deltas",
                        "cache":"warm OS cache; no disk cache eviction", "automatic_checkpoints":False,
                        "latency":"save-only, including file validation, serialization, WAL append and fsync; mutation timed separately" if a.mode=="persistence" else "single Python query through returned IDs, recall calculation excluded",
                        "builds_per_case":a.repeats if a.mode=="persistence" else 1},"results":[]}
    if sys.platform == "darwin":
        report["machine"].update({"cpu":command(["sysctl","-n","machdep.cpu.brand_string"]),
                                    "memory_bytes":int(command(["sysctl","-n","hw.memsize"])),
                                    "physical_cores":int(command(["sysctl","-n","hw.physicalcpu"]))})
    files=sorted((ROOT/"crates/recern-vector/src").rglob("*.rs"))+[ROOT/"Cargo.toml",ROOT/"Cargo.lock"]
    report["source"]["engine_files_sha256"]={str(f.relative_to(ROOT)):sha(f) for f in files}
    cases=[]
    if a.mode=="persistence":
        candidates=[(a.current_python,a.current_version,3),(a.current_python,a.current_version,2)]
        if a.baseline_python: candidates.append((a.baseline_python,"0.4.0",2))
        report["method"].update({"sizes":sizes,"dim":a.dim,"commits_per_phase":a.commits,"phases":["update","delete","no_op"],
                                   "data":"seed 20261104, numpy standard_normal float32; cosine, m=16, ef_construction=200"})
        # Alternate candidates in each repetition rather than completing all runs
        # of one version before moving to the next.
        for repeat in range(a.repeats):
            for n in sizes:
                for precision in ["f32","int8"]:
                    for py,version,layout in candidates[repeat%len(candidates):]+candidates[:repeat%len(candidates)]:
                        cases.append((py,{"records":n,"dim":a.dim,"precision":precision,"format":layout,"version":version,"repeat":repeat,"commits":a.commits}))
    else:
        source=a.data_dir/DATASETS[a.dataset][0]
        report["dataset"]={"name":a.dataset,"file":source.name,"sha256":sha(source)}
        report["method"].update({"k":10,"queries":a.queries,"query_repeats":a.repeats,"warmup_queries":min(100,a.queries),"m":16,"ef_construction":200,"ef_sweep":[10,20,40,80,160,320,640,1280],
                                 "truth":"exact recomputed on subset" if a.subset else "published ANN-Benchmarks neighbors from original f32 vectors"})
        for precision in ["f32","int8"]:
            cases.append((a.current_python,{"dataset":a.dataset,"data_dir":str(a.data_dir.resolve()),"precision":precision,"queries":a.queries,"repeats":a.repeats,"subset":a.subset,"version":a.current_version}))
    env={**os.environ,"OPENBLAS_NUM_THREADS":"1","VECLIB_MAXIMUM_THREADS":"1","OMP_NUM_THREADS":str(a.threads)}
    env.pop("PYTHONPATH",None)
    a.output.parent.mkdir(parents=True,exist_ok=True)
    for i,(py,case) in enumerate(cases):
        case.update({"mode":a.mode,"threads":a.threads,"work":str(a.work)})
        print(f"[{i+1}/{len(cases)}] {case}",flush=True)
        result=subprocess.run([str(py.absolute()),str(Path(__file__).resolve()),"--worker"],input=json.dumps(case),cwd=a.work,env=env,stdout=subprocess.PIPE,text=True)
        if result.returncode: raise RuntimeError(result.stderr or f"worker exit {result.returncode}")
        report["results"].append(json.loads(result.stdout))
        a.output.write_text(json.dumps(report,indent=2,allow_nan=False)+"\n")
    report["complete"]=True
    a.output.write_text(json.dumps(report,indent=2,allow_nan=False)+"\n")
    print(f"Complete: {a.output}",flush=True)


if __name__ == "__main__":
    worker() if sys.argv[1:] == ["--worker"] else main()
