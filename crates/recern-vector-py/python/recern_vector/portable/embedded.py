"""Durable portable operations over the existing native API."""
import threading
import uuid
from contextlib import contextmanager

from .. import Database, CorruptDatabaseError, RecernVectorError
from .model import (
    AlreadyExists, Capabilities, CollectionSpec, Conflict, CorruptData, Hit,
    InvalidArgument, NotFound, Page, PermissionDenied, Record, Unavailable,
    decode_cursor, encode_cursor, integer,
)


class EmbeddedBackend:
    def __init__(self, path, *, read_only=False):
        if type(read_only) is not bool:
            raise InvalidArgument("read_only must be a boolean")
        self.lock = threading.RLock()
        self.read_only = read_only
        self.session = str(uuid.uuid4())
        self.revision = 0
        with self.errors():
            self.db = Database.open_read_only(path) if read_only else Database.open_or_create(path)
            # Give legacy files a persistent format-2 identity before any
            # migration journal binds to this destination.
            if not read_only and self.db.identity == "0" * 32:
                self.db.save()
        self.capabilities = Capabilities("embedded", atomic_batches=True, consistent_export=True,
                                         quantization=("f32", "int8"), snapshot_pages=read_only)
    @property
    def identity(self):
        return "embedded:" + str(self.db.path) + ":" + self.db.identity

    @contextmanager
    def errors(self):
        try:
            yield
        except KeyError as e:
            raise NotFound("collection does not exist") from e
        except CorruptDatabaseError as e:
            raise CorruptData(str(e)) from e
        except (OSError, RecernVectorError) as e:
            raise Unavailable(str(e)) from e
        except ValueError as e:
            if isinstance(e, InvalidArgument):
                raise
            if "changed in another handle" in str(e):
                raise Conflict("database changed in another writer; reopen the client") from e
            raise InvalidArgument(str(e)) from e

    def list_collections(self):
        with self.lock, self.errors():
            return self.db.collection_names()

    def describe(self, name):
        with self.lock, self.errors():
            col = self.db.collection(name)
            return CollectionSpec(name, col.dim, col.metric, col.quantization), self.db.identity + ":" + name

    def _write(self, operation):
        if self.read_only:
            raise PermissionDenied("this snapshot is read-only")
        with self.lock, self.errors():
            try:
                result = operation()
                self.db.save()
            except Exception:
                # A failed durable commit must not remain visible as an
                # acknowledged in-memory mutation on this client.
                self.db = Database(self.db.path)
                self.revision += 1
                raise
            self.revision += 1
            return result

    def create(self, spec, generation=None):
        with self.lock:
            if spec.name in self.list_collections():
                raise AlreadyExists(f"collection already exists: {spec.name}")
            self._write(lambda: self.db.create_collection(spec.name, spec.dim, spec.metric, quantization=spec.quantization))
            return self.describe(spec.name)

    def drop(self, name):
        self._write(lambda: self.db.drop_collection(name))

    def upsert(self, name, records):
        return self._write(lambda: self.db.collection(name).upsert_many(
            [r.id for r in records], [r.vector for r in records], [r.metadata for r in records]))

    def get(self, name, id):
        with self.lock, self.errors():
            r = self.db.collection(name).get(id)
            return None if r is None else Record(r.id, r.vector, r.metadata)

    def delete(self, name, id):
        return self._write(lambda: self.db.collection(name).delete(id))

    def count(self, name):
        with self.lock, self.errors():
            return len(self.db.collection(name))

    def page(self, name, limit, cursor):
        with self.lock, self.errors():
            context = [self.identity, self.session, name, self.revision]
            offset = decode_cursor(cursor, context)
            if offset is None:
                offset = 0
            integer(offset, "cursor offset", 0, 2**32 - 1)
            records, next_offset = self.db.collection(name).records_page(offset, limit)
            return Page([Record(r.id, r.vector, r.metadata) for r in records],
                        None if next_offset is None else encode_cursor(context, next_offset))

    def search(self, name, vector, k, filter, exact):
        with self.lock, self.errors():
            return [Hit(h.id, h.distance, h.metadata) for h in self.db.collection(name).search(vector, k, filter=filter, exact=exact)]

    def snapshot(self):
        with self.lock:
            if self.read_only:
                return self
            # Native open_read_only takes a shared file lock, loads committed
            # WAL once, and owns that immutable state throughout the export.
            return EmbeddedBackend(self.db.path, read_only=True)
