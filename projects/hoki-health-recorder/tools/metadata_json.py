"""Decode recording metadata with unique fields and finite numeric values."""
import json
import math


def decode_metadata(data):
    def invalid_constant(value):
        raise ValueError(f'non-finite metadata number: {value}')

    def finite_float(value):
        result = float(value)
        if not math.isfinite(result):
            raise ValueError('metadata number outside finite floating-point range')
        return result

    def unique_fields(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f'duplicate metadata field: {key!r}')
            value[key] = item
        return value

    value = json.loads(data, object_pairs_hook=unique_fields,
                       parse_constant=invalid_constant, parse_float=finite_float)
    if not isinstance(value, dict):
        raise ValueError('metadata must be a JSON object')
    return value
