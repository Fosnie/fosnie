# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Bounded portable JSON contract: a flat object of required scalar fields.

Unsupported JSON Schema keywords are rejected, never silently discarded. This
subset deliberately excludes references, recursion, optional fields and unions.
"""
import json
import math


CONTRACT_REASONS = frozenset({
    "unsupported_contract", "incomplete_response", "invalid_content",
    "output_too_large", "markdown_fence", "invalid_json", "duplicate_keys",
    "fields_mismatch", "scalar_type_mismatch", "enum_mismatch", "bounds_mismatch",
})


class OutputSchemaError(ValueError):
    """Safe error text and allowlisted category; never include response data."""

    def __init__(self, message, reason="unsupported_contract"):
        super().__init__(message)
        self.reason = reason if reason in CONTRACT_REASONS else "unsupported_contract"


class DuplicateKeyError(ValueError):
    pass


def encoded_size(value):
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8"))


def typed(value, kind):
    if kind == "string":
        return isinstance(value, str)
    if kind == "boolean":
        return type(value) is bool
    if kind in ("integer", "number"):
        return type(value) in (int, float) and (type(value) is int or math.isfinite(value)) and (kind == "number" or type(value) is int or value.is_integer())
    return False


def validate_schema(schema):
    try:
        if not isinstance(schema, dict) or encoded_size(schema) > 8192:
            raise ValueError()
        if set(schema) - {"type", "properties", "required", "additionalProperties", "description"}:
            raise ValueError()
        if schema.get("type") != "object" or schema.get("additionalProperties") is not False:
            raise ValueError()
        props, required = schema.get("properties"), schema.get("required")
        if not isinstance(props, dict) or not 1 <= len(props) <= 64 or not isinstance(required, list):
            raise ValueError()
        if any(not isinstance(key, str) for key in required) or len(required) != len(props) or set(required) != set(props):
            raise ValueError()
        for node in [schema, *props.values()]:
            if not isinstance(node, dict):
                raise ValueError()
            if "description" in node and (not isinstance(node["description"], str) or len(node["description"].encode("utf-8")) > 256):
                raise ValueError()
        for key, node in props.items():
            if not key or len(key.encode("utf-8")) > 80 or not key.isascii() or any(not (c.isalnum() or c in "_-") for c in key):
                raise ValueError()
            kind = node.get("type")
            if kind not in ("string", "number", "integer", "boolean") or set(node) - {"type", "enum", "minimum", "maximum", "description"}:
                raise ValueError()
            for bound in ("minimum", "maximum"):
                if bound in node and (kind not in ("integer", "number") or not typed(node[bound], "number")):
                    raise ValueError()
            if "minimum" in node and "maximum" in node and node["minimum"] > node["maximum"]:
                raise ValueError()
            if "enum" in node:
                values = node["enum"]
                if not isinstance(values, list) or not 1 <= len(values) <= 32 or any(not typed(v, kind) for v in values):
                    raise ValueError()
                if any(v in values[:i] for i, v in enumerate(values)):
                    raise ValueError()
        return schema
    except (ValueError, TypeError, OverflowError, RecursionError):
        raise OutputSchemaError("unsupported or invalid output schema") from None


def schema_overhead(schema):
    return encoded_size(validate_schema(schema)) + 256 if schema is not None else 0


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateKeyError()
        result[key] = value
    return result


def validate_output(schema, result):
    def reject(reason):
        raise OutputSchemaError("answer does not satisfy output schema", reason)

    content = result.get("content")
    if result.get("finish_reason") != "stop" or result.get("tool_calls"):
        reject("incomplete_response")
    if not isinstance(content, str):
        reject("invalid_content")
    try:
        size = len(content.encode("utf-8"))
    except UnicodeError:
        reject("invalid_content")
    if size > 64000:
        reject("output_too_large")
    if content.lstrip().startswith("```"):
        reject("markdown_fence")
    try:
        value = json.loads(content, object_pairs_hook=unique_object,
                           parse_constant=lambda _: (_ for _ in ()).throw(ValueError()))
    except DuplicateKeyError:
        reject("duplicate_keys")
    except (ValueError, TypeError, OverflowError, RecursionError):
        reject("invalid_json")
    props = schema["properties"]
    if not isinstance(value, dict) or set(value) != set(props):
        reject("fields_mismatch")
    for key, node in props.items():
        item = value[key]
        if not typed(item, node["type"]):
            reject("scalar_type_mismatch")
        if "enum" in node and item not in node["enum"]:
            reject("enum_mismatch")
        if "minimum" in node and item < node["minimum"] or "maximum" in node and item > node["maximum"]:
            reject("bounds_mismatch")
