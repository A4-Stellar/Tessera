"""Schema evolution tests for the Kafka/Schema-Registry event path."""

from __future__ import annotations

import json
import struct

from tessera_analytics.kafka_events import (
    AvroBinaryDecoder,
    CachedSchemaRegistryClient,
    ConfluentFramingError,
    KafkaEventSource,
    SchemaRegistryClient,
)

TRANSFERRED_V1 = {
    "type": "record",
    "name": "Transferred",
    "fields": [
        {"name": "from", "type": "string"},
        {"name": "to", "type": "string"},
        {"name": "amount", "type": "long"},
    ],
}

TRANSFERRED_V2 = {
    "type": "record",
    "name": "Transferred",
    "fields": [
        {"name": "from", "type": "string"},
        {"name": "to", "type": "string"},
        {"name": "amount", "type": "long"},
        {"name": "memo", "type": ["null", "string"], "default": None},
    ],
}


def zigzag(value: int) -> bytes:
    encoded = (value << 1) ^ (value >> 63)
    out = bytearray()
    while True:
        byte = encoded & 0x7F
        encoded >>= 7
        if encoded:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def avro_string(value: str) -> bytes:
    raw = value.encode("utf-8")
    return zigzag(len(raw)) + raw


def frame(schema_id: int, payload: bytes) -> bytes:
    return b"\x00" + struct.pack(">i", schema_id) + payload


class FakeRegistry(SchemaRegistryClient):
    def __init__(self) -> None:
        super().__init__("http://unused")
        self.schemas = {1: TRANSFERRED_V1, 2: TRANSFERRED_V2}
        self.latest = TRANSFERRED_V2

    def schema_by_id(self, schema_id: int) -> dict:  # type: ignore[override]
        return self.schemas[schema_id]

    def latest_schema(self, subject: str) -> dict:  # type: ignore[override]
        return self.latest


def old_event_payload(amount: int = 1000) -> bytes:
    return avro_string("GALICE") + avro_string("GBOB") + zigzag(amount)


def test_added_nullable_field_decodes_old_event() -> None:
    registry = FakeRegistry()
    decoder = AvroBinaryDecoder(registry)
    event = decoder.decode(frame(1, old_event_payload()))

    assert event == {"from": "GALICE", "to": "GBOB", "amount": 1000, "memo": None}


def test_new_event_decodes_with_v2_writer() -> None:
    registry = FakeRegistry()
    decoder = AvroBinaryDecoder(registry)
    payload = old_event_payload(42) + zigzag(1) + avro_string("invoice-7")
    event = decoder.decode(frame(2, payload))
    assert event["memo"] == "invoice-7"


def test_unknown_writer_field_is_skipped() -> None:
    registry = FakeRegistry()
    registry.latest = TRANSFERRED_V1
    decoder = AvroBinaryDecoder(registry)
    payload = old_event_payload(7) + zigzag(1) + avro_string("ignored")
    event = decoder.decode(frame(2, payload))
    assert event == {"from": "GALICE", "to": "GBOB", "amount": 7}


def test_invalid_framing_is_rejected() -> None:
    import pytest

    decoder = AvroBinaryDecoder(FakeRegistry())
    try:
        decoder.decode(b"\x01\x00\x00\x00\x01x")
    except ConfluentFramingError:
        pass
    else:  # pragma: no cover
        raise AssertionError("expected ConfluentFramingError")
    _ = pytest  # keep import available for future parametrized cases


def test_cache_deduplicates_registry_lookups() -> None:
    class CountingCached(CachedSchemaRegistryClient):
        def __init__(self) -> None:
            super().__init__("http://unused")
            self.calls: list[str] = []

        def _get(self, path: str) -> dict:  # type: ignore[override]
            self.calls.append(path)
            return {"schema": json.dumps(TRANSFERRED_V1)}

    cached = CountingCached()
    cached.schema_by_id(1)
    cached.schema_by_id(1)
    assert cached.calls == ["/schemas/ids/1"]


class FakeMessage:
    def __init__(self, value: bytes) -> None:
        self._value = value

    def value(self) -> bytes:
        return self._value

    def error(self) -> None:
        return None


class FakeConsumer:
    def __init__(self, messages: list[bytes]) -> None:
        self._messages = [FakeMessage(message) for message in messages]

    def consume(self, _count: int, _timeout: float) -> list[FakeMessage]:
        return [self._messages.pop(0)] if self._messages else []


def test_kafka_source_yields_typed_events() -> None:
    source = KafkaEventSource(
        FakeConsumer([frame(1, old_event_payload())]), AvroBinaryDecoder(FakeRegistry())
    )
    events = list(source.events(limit=1))
    assert events[0]["amount"] == 1000
    assert events[0]["memo"] is None
