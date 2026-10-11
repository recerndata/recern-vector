"""Filter validation and a reversible, typed Qdrant payload projection.

Raw Qdrant payload filters have array-membership/null semantics which differ
from Recern. Index whole values, not individual array elements, under encoded
object-only paths. Keep original JSON separately for lossless metadata reads.
"""
from .model import InvalidArgument, dumps, metadata_value


def validate_filter(value, depth=0):
    if value is None and depth == 0:
        return None
    if depth > 64 or type(value) is not dict:
        raise InvalidArgument("filter must be an object with at most 64 nesting levels")
    value = metadata_value(value) if depth == 0 else value
    for field, condition in value.items():
        if field.startswith("$"):
            if field in ("$and", "$or"):
                if type(condition) is not list or not condition:
                    raise InvalidArgument(f"{field} requires a nonempty list")
                for item in condition:
                    validate_filter(item, depth + 1)
            elif field == "$not":
                validate_filter(condition, depth + 1)
            else:
                raise InvalidArgument(f"unsupported filter operator {field}")
        elif type(condition) is dict and any(key.startswith("$") for key in condition):
            for op, arg in condition.items():
                if op == "$eq":
                    pass
                elif op == "$in" and type(arg) is list:
                    pass
                elif op in ("$gt", "$gte", "$lt", "$lte") and type(arg) in (int, float):
                    pass
                else:
                    raise InvalidArgument(f"unsupported filter operator or argument: {op}")
    return value


def path_key(path):
    # Hex is injective and contains none of Qdrant's path/array syntax.
    return "p" + path.encode("utf-8").hex()


def _typed(value):
    if value is None:
        return ["null"]
    if type(value) is bool:
        return ["bool", value]
    if type(value) is int:
        return ["int", str(value)]
    if type(value) is float:
        return ["float", (0.0 if value == 0 else value).hex()]
    if type(value) is str:
        return ["string", value]
    if type(value) is list:
        return ["array", [_typed(x) for x in value]]
    return ["object", [[k, _typed(v)] for k, v in sorted(value.items())]]


def equality_key(value):
    # Recern compares top-level JSON numbers as f64; structural equality
    # retains nested number types. bool is never the number 0 or 1.
    if type(value) in (int, float):
        number = float(value)
        return dumps(["number", (0.0 if number == 0 else number).hex()])
    return dumps(_typed(value))


def projection(metadata):
    eq, numeric = {}, {}

    def visit(value, prefix):
        if type(value) is not dict:
            return
        for key, item in value.items():
            if "." in key:
                continue  # The native dotted-path lookup cannot address this key.
            path = key if prefix is None else prefix + "." + key
            encoded = path_key(path)
            eq[encoded] = equality_key(item)
            if type(item) in (int, float):
                numeric[encoded] = float(item)
            visit(item, path)

    visit(metadata, None)
    return {"eq": eq, "num": numeric}


def qdrant_filter(value):
    if value is None:
        return None
    parts = []
    for field, condition in value.items():
        if field in ("$and", "$or"):
            parts.append({"must" if field == "$and" else "should": [qdrant_filter(c) for c in condition]})
        elif field == "$not":
            parts.append({"must_not": [qdrant_filter(condition)]})
        else:
            ops = condition if type(condition) is dict and any(k.startswith("$") for k in condition) else {"$eq": condition}
            ranges = {}
            for op, arg in ops.items():
                if op == "$eq":
                    parts.append({"key": "_recern.eq." + path_key(field), "match": {"value": equality_key(arg)}})
                elif op == "$in":
                    # An empty $in is false, including inside NOT/OR.
                    matches = [{"key": "_recern.eq." + path_key(field), "match": {"value": equality_key(x)}} for x in arg]
                    parts.append({"should": matches} if matches else {"must_not": [{}]})
                else:
                    ranges[op[1:]] = float(arg)
            if ranges:
                parts.append({"key": "_recern.num." + path_key(field), "range": ranges})
    return {"must": parts} if parts else {}
