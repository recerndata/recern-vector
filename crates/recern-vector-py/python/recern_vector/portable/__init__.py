"""Portable API v1 for embedded Recern and Qdrant.

The low-level recern_vector.Database API keeps its explicit save/checkpoint
semantics. This client acknowledges embedded writes only after a durable save.
"""
from .client import Client, Collection
from .migration import export_snapshot, import_snapshot, inspect_export
from .model import (
    API_VERSION, PROFILE, AlreadyExists, Capabilities, CollectionSpec, Conflict,
    CorruptData, Hit, InvalidArgument, NotFound, Page, PermissionDenied,
    PortableError, Record, Unavailable, Unsupported,
)

__all__ = [
    "API_VERSION", "PROFILE", "Client", "Collection", "CollectionSpec", "Record",
    "Hit", "Page", "Capabilities", "PortableError", "InvalidArgument", "NotFound",
    "AlreadyExists", "Conflict", "CorruptData", "Unavailable", "PermissionDenied", "Unsupported",
    "export_snapshot", "import_snapshot", "inspect_export",
]
