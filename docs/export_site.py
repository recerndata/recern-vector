"""Renders docs/*.md to HTML for recern.net/vector/docs.

    uv run --with markdown --with pygments python docs/export_site.py <site>/lib/vector-docs.json

Links between pages become /vector/docs/<slug>; links to other files in the
repository point to GitHub.
"""

import html
import json
import posixpath
import re
import subprocess
import sys
from pathlib import Path

import markdown

DOCS = Path(__file__).resolve().parent
REPO_URL = "https://github.com/recerndata/recern-vector"
# Sidebar order. The README is the index page (slug "").
PAGES = [
    ("", "README.md", "Documentation for Recern Vector, an embedded single-file vector database for Python, Rust and the command line."),
    ("getting-started", "getting-started.md", "Install Recern Vector and create a first vector database from Python, Rust or the command line."),
    ("concepts", "concepts.md", "Databases, collections, records, distance metrics, the HNSW index, saving and threads in Recern Vector."),
    ("filters", "filters.md", "Metadata filter syntax in Recern Vector and how filtered vector searches are planned."),
    ("inspection", "inspection.md", "Use explain, stats and estimate_recall to see how Recern Vector searches run and to choose ef."),
    ("python", "python.md", "Reference for the recern_vector Python package: Database, Collection, results and exceptions."),
    ("rust", "rust.md", "A map of the recern-vector Rust crate: Database, Collection, filters and introspection."),
    ("cli", "cli.md", "The recern-vector command: create databases, insert JSON Lines, query, inspect, measure recall and compact."),
    ("file-format", "file-format.md", "The layout of a Recern Vector .rvec file, its checksum and atomic saves."),
    ("limitations", "limitations.md", "What the Recern Vector prototype does not do yet, and the roadmap to 0.1."),
]
SLUGS = {file: slug for slug, file, _ in PAGES}
SITE_URL = "https://recern.net"


def rewrite_link(href: str) -> str:
    if href.startswith(SITE_URL + "/"):
        return href[len(SITE_URL):]
    if href == SITE_URL:
        return "/"
    if re.match(r"^[a-z]+:", href) or href.startswith("#"):
        return href
    path, _, anchor = href.partition("#")
    anchor = f"#{anchor}" if anchor else ""
    if path in SLUGS:
        slug = SLUGS[path]
        return f"/vector/docs/{slug}{anchor}" if slug else f"/vector/docs{anchor}"
    target = posixpath.normpath(posixpath.join("docs", path))
    kind = "tree" if path.endswith("/") or "." not in posixpath.basename(target) else "blob"
    return f"{REPO_URL}/{kind}/main/{target}{anchor}"


def render(source: str) -> dict:
    md = markdown.Markdown(
        extensions=["fenced_code", "tables", "toc", "codehilite"],
        extension_configs={
            "codehilite": {"guess_lang": False, "css_class": "highlight"},
            "toc": {"toc_depth": "2-3"},
        },
    )
    body = md.convert(source)
    title_match = re.search(r"<h1[^>]*>(.*?)</h1>", body, re.S)
    title = html.unescape(re.sub(r"<[^>]+>", "", title_match.group(1))).strip()
    body = body[: title_match.start()] + body[title_match.end() :]
    body = re.sub(r'href="([^"]+)"', lambda m: f'href="{html.escape(rewrite_link(html.unescape(m.group(1))))}"', body)
    # External links open in a new tab.
    body = re.sub(r'<a href="(https?://[^"]+)"', r'<a href="\1" target="_blank" rel="noopener noreferrer"', body)
    headings = [
        {"id": item["id"], "title": html.unescape(item["name"])}
        for top in md.toc_tokens
        for item in (top["children"] if top["level"] == 1 else [top])
        if item["level"] == 2
    ]
    return {"title": title, "html": body.strip(), "headings": headings}


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=DOCS, capture_output=True, text=True).stdout.strip()
    pages = []
    for slug, file, description in PAGES:
        page = render((DOCS / file).read_text())
        pages.append({"slug": slug, "file": f"docs/{file}", "description": description, **page})
    out = Path(sys.argv[1])
    out.write_text(json.dumps({"commit": commit, "repo": REPO_URL, "pages": pages}, indent=1, ensure_ascii=False) + "\n")
    print(f"wrote {len(pages)} pages to {out}")


if __name__ == "__main__":
    main()
