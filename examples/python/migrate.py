"""Export a committed Recern database and import it into another backend.

The destination must not contain these collections. Use --dry-run first and
--resume after an interrupted import. The journal never contains credentials.
"""
import argparse
import os

from recern_vector.portable import Client, export_snapshot, import_snapshot

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("export", help="directory containing the versioned export")
parser.add_argument("--source", help="create this export from a committed .rvec database")
parser.add_argument("--target-file")
parser.add_argument("--qdrant")
parser.add_argument("--namespace", default="recern")
parser.add_argument("--dry-run", action="store_true")
parser.add_argument("--resume", action="store_true")
args = parser.parse_args()
if args.target_file and args.qdrant:
    parser.error("choose one destination")
if args.source:
    print(export_snapshot(Client.embedded(args.source, read_only=True), args.export))
if args.target_file or args.qdrant:
    target = (Client.qdrant(args.qdrant, api_key=os.getenv("RECERN_QDRANT_API_KEY"), namespace=args.namespace)
              if args.qdrant else Client.embedded(args.target_file))
    print(import_snapshot(args.export, target, dry_run=args.dry_run, resume=args.resume,
                          progress=lambda name, done, total: print(f"{name}: {done}/{total}")))
