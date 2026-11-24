"""Type-safe Soroban event consumption from Kafka via the Schema Registry.

The analytics SDK consumes decoded contract events directly from the Kafka
``tessera.contract-events`` topic. Messages use the Confluent wire format::

    byte 0          magic (0)
    bytes 1..5      schema id (big-endian int32)
    bytes 5..       Avro binary payload (writer schema)

A :class:`SchemaRegistryClient` resolves the schema id (and the latest reader
schema for a subject), and :class:`AvroBinaryDecoder` performs schema
evolution: fields added with a default in the reader schema are populated for
older events, and unknown writer fields are skipped. This is the Python half
of the Rust ``indexer/event_decoder.rs`` path — both read the same registered
Avro schemas produced from the Soroban ABI.

The Kafka consumer itself is duck-typed (any object exposing ``consume()`` or
``poll()`` returning objects with ``value()``), so ``confluent-kafka`` is an
optional dependency rather than a hard requirement. Unit tests use a small
in-memory fake.
"""

from __future__ import annotations

import json
import struct
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any, Iterable, Mapping, Optional, Sequence

__all__ = [
    "SchemaRegistryClient",
    "CachedSchemaRegistryClient",
    "AvroBinaryDecoder",
    "KafkaEventSource",
    "ConfluentFramingError",
    "SchemaResolutionError",
]


class ConfluentFramingError(ValueError):
    """Raised when a Kafka message is not valid Confluent/Avro framing."""


class SchemaResolutionError(ValueError):
    """Raised when writer and reader schemas cannot be resolved."""


def _decode_zigzag(value: int) -> int:
    return (value >> 1) ^ -(value & 1)


def _read_long(buf: bytes, pos: int) -> tuple[int, int]:
    """Read an Avro variable-length zigzag-encoded int/long."""
    shift = 0
    result = 0
    while True:
        if pos >= len(buf):
            raise ConfluentFramingError("truncated Avro long")
        byte = buf[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return _decode_zigzag(result), pos
        shift += 7
        if shift > 63:
            raise ConfluentFramingError("Avro long overflow")


def _schema_name(schema: Any) -> str:
    if isinstance(schema, str):
        return schema
    if isinstance(schema, list):
        return "union"
    if isinstance(schema, dict):
        name = schema.get("type")
        if isinstance(name, str):
            if name in {"record", "error", "enum", "fixed", "array", "map"}:
                return name
            return name
    raise SchemaResolutionError(f"unsupported Avro schema: {schema!r}")


def _is_nullable(schema: Any) -> bool:
    return isinstance(schema, list) and "null" in schema


class SchemaRegistryClient:
    """Minimal Confluent Schema Registry HTTP client (stdlib only)."""

    def __init__(self, base_url: str, timeout: float = 5.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def _get(self, path: str) -> Any:
        request = urllib.request.Request(
            f"{self.base_url}{path}", headers={"Accept": "application/vnd.schemaregistry.v1+json"}
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return json.loads(response.read().decode("utf-8"))
        except urllib.error.HTTPError as exc:  # pragma: no cover - network path
            raise SchemaResolutionError(f"schema registry {path}: HTTP {exc.code}") from exc
        except urllib.error.URLError as exc:  # pragma: no cover - network path
            raise SchemaResolutionError(f"schema registry {path}: {exc.reason}") from exc

    def schema_by_id(self, schema_id: int) -> dict[str, Any]:
        payload = self._get(f"/schemas/ids/{schema_id}")
        raw = payload.get("schema") if isinstance(payload, dict) else None
        if not isinstance(raw, str):
            raise SchemaResolutionError(f"schema id {schema_id} missing 'schema'")
        schema = json.loads(raw)
        if not isinstance(schema, dict):
            raise SchemaResolutionError(f"schema id {schema_id} is not an Avro record")
        return schema

    def latest_schema(self, subject: str) -> dict[str, Any]:
        payload = self._get(f"/subjects/{subject}/versions/latest")
        raw = payload.get("schema") if isinstance(payload, dict) else None
        if not isinstance(raw, str):
            raise SchemaResolutionError(f"subject {subject} missing 'schema'")
        schema = json.loads(raw)
        if not isinstance(schema, dict):
            raise SchemaResolutionError(f"subject {subject} is not an Avro record")
        return schema


class CachedSchemaRegistryClient(SchemaRegistryClient):
    """Schema client with in-process caching of ids and subjects."""

    def __init__(self, base_url: str, timeout: float = 5.0) -> None:
        super().__init__(base_url, timeout)
        self._by_id: dict[int, dict[str, Any]] = {}
        self._by_subject: dict[str, dict[str, Any]] = {}

    def schema_by_id(self, schema_id: int) -> dict[str, Any]:
        if schema_id not in self._by_id:
            self._by_id[schema_id] = super().schema_by_id(schema_id)
        return self._by_id[schema_id]

    def latest_schema(self, subject: str) -> dict[str, Any]:
        if subject not in self._by_subject:
            self._by_subject[subject] = super().latest_schema(subject)
        return self._by_subject[subject]


class AvroBinaryDecoder:
    """Decode Confluent-framed Avro bytes with writer/reader schema evolution."""

    def __init__(self, registry: SchemaRegistryClient, subject: str = "tessera.contract-events-value") -> None:
        self.registry = registry
        self.subject = subject

    def decode(self, message: bytes, reader_schema: Optional[dict[str, Any]] = None) -> dict[str, Any]:
        if len(message) < 6 or message[0] != 0:
            raise ConfluentFramingError("expected Confluent magic byte 0 and a schema id")
        schema_id = struct.unpack(">i", message[1:5])[0]
        writer_schema = self.registry.schema_by_id(schema_id)
        if reader_schema is None:
            reader_schema = self.registry.latest_schema(self.subject)
        value, _ = self._resolve(writer_schema, reader_schema, message, 5)
        if not isinstance(value, dict):
            raise SchemaResolutionError("event payload is not an Avro record")
        return value

    def _skip(self, writer: Any, buf: bytes, pos: int) -> int:
        """Advance past one value written with ``writer`` but unread by the reader."""
        if writer == "null":
            return pos
        kind = _schema_name(writer)
        if kind == "union":
            branches = writer if isinstance(writer, list) else []
            branch_index, pos = _read_long(buf, pos)
            if branch_index < 0 or branch_index >= len(branches):
                raise SchemaResolutionError(f"union branch {branch_index} out of range")
            return self._skip(branches[branch_index], buf, pos)
        if kind in {"int", "long", "enum"}:
            _, pos = _read_long(buf, pos)
            return pos
        if kind == "boolean":
            return pos + 1
        if kind == "float":
            return pos + 4
        if kind == "double":
            return pos + 8
        if kind in {"bytes", "string"}:
            length, pos = _read_long(buf, pos)
            return pos + length
        if kind == "fixed":
            return pos + int(writer.get("size", 0))
        if kind in {"record", "error"}:
            for field in writer.get("fields", []):
                pos = self._skip(field.get("type"), buf, pos)
            return pos
        if kind == "array":
            while True:
                count, pos = _read_long(buf, pos)
                if count == 0:
                    return pos
                if count < 0:
                    count = -count
                    _, pos = _read_long(buf, pos)
                for _ in range(count):
                    pos = self._skip(writer.get("items"), buf, pos)
        if kind == "map":
            while True:
                count, pos = _read_long(buf, pos)
                if count == 0:
                    return pos
                if count < 0:
                    count = -count
                    _, pos = _read_long(buf, pos)
                for _ in range(count):
                    length, pos = _read_long(buf, pos)
                    pos += length
                    pos = self._skip(writer.get("values"), buf, pos)
        raise SchemaResolutionError(f"cannot skip Avro type {kind}")

    def _resolve(
        self, writer: Any, reader: Any, buf: bytes, pos: int
    ) -> tuple[Any, int]:
        """Decode a value written with ``writer`` as ``reader`` expects it."""
        writer_type = _schema_name(writer)
        reader_type = _schema_name(reader)

        if writer_type == "union":
            branch_index, pos = _read_long(buf, pos)
            branches = writer if isinstance(writer, list) else []
            if branch_index < 0 or branch_index >= len(branches):
                raise SchemaResolutionError(f"union branch {branch_index} out of range")
            writer = branches[branch_index]
            writer_type = _schema_name(writer)
            if reader_type == "union" and writer in reader:
                reader = writer
                reader_type = writer_type

        if reader_type == "null":
            return None, pos
        if reader_type == "union":
            # Choose the first non-null branch when old data cannot be null.
            candidates = [branch for branch in reader if branch != "null"] or ["null"]
            return self._resolve(writer, candidates[0], buf, pos)

        if writer_type == "null":
            if _is_nullable(reader) is False and reader_type != "null":
                raise SchemaResolutionError("null value for non-null reader field")
            return None, pos

        if writer_type != reader_type:
            # Widening int -> long (and float -> double) is lossless.
            if (writer_type, reader_type) in {("int", "long"), ("float", "double")}:
                pass
            elif writer_type == "long" and reader_type in {"int", "long"}:
                pass
            else:
                raise SchemaResolutionError(
                    f"cannot resolve writer {writer_type} as reader {reader_type}"
                )

        if reader_type in {"int", "long"}:
            value, pos = _read_long(buf, pos)
            return value, pos
        if reader_type == "boolean":
            if pos >= len(buf):
                raise ConfluentFramingError("truncated boolean")
            value = buf[pos] != 0
            return value, pos + 1
        if reader_type == "float":
            if pos + 4 > len(buf):
                raise ConfluentFramingError("truncated float")
            return struct.unpack("<f", buf[pos : pos + 4])[0], pos + 4
        if reader_type == "double":
            if pos + 8 > len(buf):
                raise ConfluentFramingError("truncated double")
            return struct.unpack("<d", buf[pos : pos + 8])[0], pos + 8
        if reader_type in {"bytes", "string"}:
            length, pos = _read_long(buf, pos)
            if length < 0 or pos + length > len(buf):
                raise ConfluentFramingError("invalid Avro bytes length")
            raw = buf[pos : pos + length]
            pos += length
            return (raw.decode("utf-8") if reader_type == "string" else raw), pos
        if reader_type == "fixed":
            size = reader.get("size") if isinstance(reader, dict) else None
            if not isinstance(size, int) or pos + size > len(buf):
                raise ConfluentFramingError("invalid Avro fixed size")
            return buf[pos : pos + size], pos + size
        if reader_type == "enum":
            index, pos = _read_long(buf, pos)
            symbols = reader.get("symbols", []) if isinstance(reader, dict) else []
            if index < 0 or index >= len(symbols):
                raise SchemaResolutionError(f"enum index {index} out of range")
            return symbols[index], pos
        if reader_type in {"record", "error"}:
            writer_fields: Sequence[Mapping[str, Any]] = (
                writer.get("fields", []) if isinstance(writer, dict) else []
            )
            reader_fields: Sequence[Mapping[str, Any]] = (
                reader.get("fields", []) if isinstance(reader, dict) else []
            )
            reader_by_name = {str(field.get("name")): field for field in reader_fields}
            values: dict[str, Any] = {}
            # Walk the writer's fields in wire order: decode those the reader
            # knows and skip the ones it does not.
            for field in writer_fields:
                name = str(field.get("name"))
                if name in reader_by_name:
                    values[name], pos = self._resolve(
                        field.get("type"), reader_by_name[name].get("type"), buf, pos
                    )
                else:
                    pos = self._skip(field.get("type"), buf, pos)
            # Fields only present in the reader schema come from their default.
            for field in reader_fields:
                name = str(field.get("name"))
                if name not in values:
                    if "default" not in field:
                        raise SchemaResolutionError(f"missing field '{name}' has no default")
                    values[name] = field["default"]
            return values, pos
        if reader_type == "array":
            items = reader.get("items") if isinstance(reader, dict) else None
            result: list[Any] = []
            while True:
                count, pos = _read_long(buf, pos)
                if count == 0:
                    break
                if count < 0:
                    count = -count
                    _, pos = _read_long(buf, pos)  # block byte size
                for _ in range(count):
                    item, pos = self._resolve(writer.get("items"), items, buf, pos)
                    result.append(item)
            return result, pos
        if reader_type == "map":
            values_type = reader.get("values") if isinstance(reader, dict) else None
            result_map: dict[str, Any] = {}
            while True:
                count, pos = _read_long(buf, pos)
                if count == 0:
                    break
                if count < 0:
                    count = -count
                    _, pos = _read_long(buf, pos)
                for _ in range(count):
                    length, pos = _read_long(buf, pos)
                    key = buf[pos : pos + length].decode("utf-8")
                    pos += length
                    result_map[key], pos = self._resolve(
                        writer.get("values"), values_type, buf, pos
                    )
            return result_map, pos
        raise SchemaResolutionError(f"unsupported Avro type {reader_type}")


@dataclass
class KafkaEventSource:
    """Iterate decoded events from a Kafka consumer.

    ``consumer`` must expose ``consume(timeout)`` (confluent-kafka) or
    ``poll(timeout)`` returning objects with ``value()``/``error()``.
    """

    consumer: Any
    decoder: AvroBinaryDecoder
    event_name_field: str = "event_name"

    def events(self, limit: Optional[int] = None, timeout: float = 1.0) -> Iterable[dict[str, Any]]:
        consumed = 0
        while limit is None or consumed < limit:
            if hasattr(self.consumer, "consume"):
                messages = self.consumer.consume(1, timeout)
                message = messages[0] if messages else None
            else:  # pragma: no cover - alternate consumer
                message = self.consumer.poll(timeout)
            if message is None:
                return
            error = getattr(message, "error", lambda: None)()
            if error:
                # Poison pill: surface it rather than silently dropping data.
                raise SchemaResolutionError(f"Kafka message error: {error}")
            raw = message.value() if callable(getattr(message, "value", None)) else message
            if not isinstance(raw, (bytes, bytearray)):
                continue
            decoded = self.decoder.decode(bytes(raw))
            consumed += 1
            yield decoded
