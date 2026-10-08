"""Builds the public benchmark report page (bench/report.html) from
bench/results/*.json and bench/report_template.html.

    python3 bench/report_page.py                      # bench/report.html
    python3 bench/report_page.py --site-json <path>   # data for the recern.net page
"""

import html
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
TARGETS = [0.90, 0.95, 0.99]
# Chart and table order; the default IVF_PQ run is discussed in the text only.
ENGINES = [
    ("recern", "Recern Vector", "HNSW"),
    ("faiss-hnsw", "faiss", "HNSWFlat"),
    ("lancedb-hnsw", "LanceDB", "IVF_HNSW_SQ"),
    ("lancedb-pq-tuned", "LanceDB", "IVF_PQ, tuned"),
    ("sqlite-vec", "sqlite-vec", "exact scan"),
]
DATASETS = [("sift", "SIFT1M"), ("glove", "GloVe-100")]


def best_at(runs, target):
    ok = [r for r in runs if r["recall"] >= target]
    return max(ok, key=lambda r: r["qps"]) if ok else None


def fmt_qps(q):
    return f"{q:,.0f}"


def fmt_s(s):
    return f"{s:,.0f}&nbsp;s" if s >= 10 else f"{s:.1f}&nbsp;s"


def fmt_mb(b):
    return f"{b / 2**20:,.0f}&nbsp;MB"


def qps_table(report):
    rows = []
    for key, name, index in ENGINES:
        e = report["engines"][key]
        cells = []
        for t in TARGETS:
            r = best_at(e["runs"], t)
            cells.append(
                f'<td class="num">{fmt_qps(r["qps"])}<span class="sub">{r["p50_ms"]:.2f}&nbsp;ms</span></td>'
                if r
                else '<td class="num none">not reached</td>'
            )
        cls = ' class="ours"' if key == "recern" else ""
        rows.append(
            f'<tr{cls}><th scope="row">{name}<span class="sub">{html.escape(index)}</span></th>{"".join(cells)}</tr>'
        )
    head = "".join(f'<th scope="col" class="num">recall ≥ {t:.2f}</th>' for t in TARGETS)
    return f'<table><thead><tr><th scope="col">Engine</th>{head}</tr></thead><tbody>{"".join(rows)}</tbody></table>'


def build_table(reports):
    rows = []
    for (ds, label), report in zip(DATASETS, reports):
        e, s = report["engines"], report["build_scaling"]["seconds"]
        cores = report["build_scaling"]["cores"]
        lance = e["lancedb-hnsw"]
        rows.append(
            f'<tr><th scope="row">{label}</th>'
            f'<td class="num">{fmt_s(s["recern_1"])}</td><td class="num">{fmt_s(s[f"recern_{cores}"])}</td>'
            f'<td class="num">{fmt_s(e["faiss-hnsw"]["build_s"])}</td><td class="num">{fmt_s(s[f"faiss_{cores}"])}</td>'
            f'<td class="num">{fmt_s(lance["build_s"])}</td></tr>'
        )
    return (
        "<table><thead><tr><th scope=\"col\">Dataset</th>"
        '<th scope="col" class="num">Recern, 1&nbsp;thread</th>'
        f'<th scope="col" class="num">Recern, {cores}&nbsp;threads</th>'
        '<th scope="col" class="num">faiss, 1&nbsp;thread</th>'
        f'<th scope="col" class="num">faiss, {cores}&nbsp;threads</th>'
        '<th scope="col" class="num">LanceDB IVF_HNSW_SQ</th></tr></thead>'
        f'<tbody>{"".join(rows)}</tbody></table>'
    )


def disk_table(reports):
    head = "".join(f'<th scope="col" class="num">{label}</th>' for _, label in DATASETS)
    rows = []
    for key, name, index in ENGINES:
        cells = "".join(f'<td class="num">{fmt_mb(r["engines"][key]["disk_bytes"])}</td>' for r in reports)
        rows.append(f'<tr><th scope="row">{name}<span class="sub">{html.escape(index)}</span></th>{cells}</tr>')
    return f'<table><thead><tr><th scope="col">Engine</th>{head}</tr></thead><tbody>{"".join(rows)}</tbody></table>'


def chart_data(reports):
    data = {}
    for (ds, label), report in zip(DATASETS, reports):
        data[ds] = {
            "label": label,
            "engines": [
                {
                    "key": key,
                    "name": name,
                    "index": index,
                    "points": [
                        {"c": r["config"], "r": round(r["recall"], 4), "q": round(r["qps"], 2), "p": round(r["p50_ms"], 3)}
                        for r in report["engines"][key]["runs"]
                    ],
                }
                for key, name, index in ENGINES
            ],
        }
    return data


def site_data(reports):
    """Compact results for the recern.net report page."""
    datasets = []
    for (ds, label), report in zip(DATASETS, reports):
        engines = []
        for key, name, index in ENGINES:
            e = report["engines"][key]
            engines.append({
                "key": key,
                "name": name,
                "index": index,
                "buildSeconds": round(e["build_s"], 1),
                "diskBytes": e["disk_bytes"],
                "runs": [
                    {"c": r["config"], "r": round(r["recall"], 4), "q": round(r["qps"], 2), "p": round(r["p50_ms"], 3)}
                    for r in e["runs"]
                ],
            })
        s = report["build_scaling"]
        datasets.append({
            "key": ds,
            "label": label,
            "records": report["records"],
            "dim": report["dim"],
            "metric": report["metric"],
            "queries": report["queries"],
            "engines": engines,
            "build": {
                "cores": s["cores"],
                "recern1": round(s["seconds"]["recern_1"], 1),
                "recernAll": round(s["seconds"][f"recern_{s['cores']}"], 1),
                "faiss1": round(report["engines"]["faiss-hnsw"]["build_s"], 1),
                "faissAll": round(s["seconds"][f"faiss_{s['cores']}"], 1),
            },
            "lancedbPqDefaultMaxRecall": round(max(r["recall"] for r in report["engines"]["lancedb-pq"]["runs"]), 2),
        })
    return {"date": "2026-10-07", "machine": reports[0]["machine"], "datasets": datasets}


def main():
    reports = [json.loads((ROOT / "results" / f"{ds}.json").read_text()) for ds, _ in DATASETS]
    if len(sys.argv) > 2 and sys.argv[1] == "--site-json":
        Path(sys.argv[2]).write_text(json.dumps(site_data(reports), indent=1) + "\n")
        print(f"wrote {sys.argv[2]}")
        return
    sift, glove = reports

    def ratio(report, target):
        ours = best_at(report["engines"]["recern"]["runs"], target)
        ref = best_at(report["engines"]["faiss-hnsw"]["runs"], target)
        return ours["qps"] / ref["qps"]

    sift_ef80 = next(r for r in sift["engines"]["recern"]["runs"] if r["config"] == "ef=80")
    values = {
        "SIFT_95_RATIO": f"{ratio(sift, 0.95):.2f}",
        "GLOVE_95_RATIO": f"{ratio(glove, 0.95):.2f}",
        "SIFT_BUILD": f"{sift['engines']['recern']['build_s']:.0f}",
        "SIFT_EF80_P50": f"{sift_ef80['p50_ms']:.2f}",
        "SIFT_EF80_RECALL": f"{sift_ef80['recall']:.3f}",
        "SIFT_QPS_TABLE": qps_table(sift),
        "GLOVE_QPS_TABLE": qps_table(glove),
        "BUILD_TABLE": build_table(reports),
        "DISK_TABLE": disk_table(reports),
        "CPU": sift["machine"]["cpu"],
        "MEMORY": str(sift["machine"]["memory_gb"]),
        "OS": sift["machine"]["os"],
        "PYTHON": sift["machine"]["python"],
        "CORES": str(sift["build_scaling"]["cores"]),
    }
    page = (ROOT / "report_template.html").read_text()
    for key, value in values.items():
        page = page.replace("{{" + key + "}}", value)
    page = page.replace("/*DATA*/null", json.dumps(chart_data(reports), separators=(",", ":")))
    assert "{{" not in page, "unfilled placeholder"
    (ROOT / "report.html").write_text(page)
    print(f"wrote {ROOT / 'report.html'}")


if __name__ == "__main__":
    main()
