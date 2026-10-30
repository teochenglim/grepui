#!/usr/bin/env python3
"""Send demo traces and logs to GreptimeDB over OTLP/HTTP (stdlib only).

    python3 scripts/seed.py [--url http://127.0.0.1:4000] [--traces 300] [--minutes 60] [--seed 7]

Writes the real `opentelemetry_traces` / `opentelemetry_logs` tables, so grepui is exercised
against the schema GreptimeDB creates itself. grepui never writes; this script is the writer.
"""
import argparse, json, random, time, urllib.request

SERVICES = ["frontend", "cart", "checkout", "payment", "inventory", "postgres"]
CALLS = {"frontend": ["cart", "checkout"], "checkout": ["payment", "inventory"], "cart": ["postgres"], "inventory": ["postgres"]}
OPS = {
    "frontend": ["GET /", "GET /product/{id}", "POST /checkout"],
    "cart": ["GET /cart", "POST /cart/items"], "checkout": ["PlaceOrder", "ValidateCart"],
    "payment": ["Charge", "Refund"], "inventory": ["Reserve", "Lookup"],
    "postgres": ["SELECT carts", "INSERT orders", "UPDATE stock"],
}
KIND_SERVER, KIND_CLIENT = 2, 3


# --- minimal protobuf encoder: GreptimeDB's OTLP endpoint accepts only application/x-protobuf ---
def varint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        out.append(b | (0x80 if n else 0))
        if not n:
            return bytes(out)


def f_bytes(num, b):
    return varint(num << 3 | 2) + varint(len(b)) + b


def f_str(num, s):
    return f_bytes(num, s.encode())


def f_var(num, n):
    return varint(num << 3) + varint(n)


def f_fix64(num, n):
    return varint(num << 3 | 1) + n.to_bytes(8, "little")


def attr(k, v):
    any_value = f_str(1, v) if isinstance(v, str) else f_var(3, v)
    return f_str(1, k) + f_bytes(2, any_value)


def post(url, path, body, headers):
    req = urllib.request.Request(url + path, body, {"Content-Type": "application/x-protobuf", **headers})
    urllib.request.urlopen(req, timeout=30).read()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:4000")
    ap.add_argument("--traces", type=int, default=300)
    ap.add_argument("--minutes", type=int, default=60)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--db", default="public", help="GreptimeDB database (an org in grepui); created if missing")
    a = ap.parse_args()
    rnd = random.Random(a.seed)
    if a.db != "public":
        urllib.request.urlopen(urllib.request.Request(a.url + "/v1/sql", ("sql=CREATE DATABASE IF NOT EXISTS " + a.db).encode()), timeout=30).read()
    db_header = {"x-greptime-db-name": a.db}
    now = int(time.time() * 1e9)
    spans = {s: [] for s in SERVICES}
    logs = {s: [] for s in SERVICES}

    def walk(svc, trace, parent, start, depth, err_chain):
        sid = "%016x" % rnd.getrandbits(64)
        dur = int(rnd.lognormvariate(16.5, 0.7))  # ~15 ms median
        fails = err_chain or (svc == "payment" and rnd.random() < 0.12)
        t = start + rnd.randint(0, 2_000_000)
        for child in CALLS.get(svc, []):
            if rnd.random() < 0.8:
                walk(child, trace, sid, t, depth + 1, fails and rnd.random() < 0.7)
                t += rnd.randint(1_000_000, 8_000_000)
        end = max(t + dur, start + dur)
        op = rnd.choice(OPS[svc])
        spans[svc].append(
            f_bytes(1, bytes.fromhex(trace)) + f_bytes(2, bytes.fromhex(sid)) + f_bytes(4, bytes.fromhex(parent or ""))
            + f_str(5, op) + f_var(6, KIND_SERVER if svc != "postgres" else KIND_CLIENT)
            + f_fix64(7, start) + f_fix64(8, end)
            + f_bytes(9, attr("http.route", op)) + f_bytes(9, attr("http.status_code", 500 if fails else 200))
            + f_bytes(15, f_var(3, 2 if fails else 1))
        )
        sev = ("ERROR", 17) if fails else (("WARN", 13) if rnd.random() < 0.08 else ("INFO", 9))
        body = f"{op} {'failed: upstream timeout' if fails else 'completed'} in {dur // 1_000_000} ms"
        logs[svc].append(
            f_fix64(1, start + 1000) + f_var(2, sev[1]) + f_str(3, sev[0]) + f_bytes(5, f_str(1, body))
            + f_bytes(6, attr("http.method", op.split(" ")[0] if " " in op else "RPC"))
            + f_bytes(9, bytes.fromhex(trace)) + f_bytes(10, bytes.fromhex(sid))
        )
        return end

    for _ in range(a.traces):
        start = now - rnd.randint(0, a.minutes * 60) * 1_000_000_000
        walk("frontend", "%032x" % rnd.getrandbits(128), None, start, 0, False)

    def res(svc):
        return f_bytes(1, attr("service.name", svc)) + f_bytes(1, attr("service.version", "1.4.2"))

    scope = f_bytes(1, f_str(1, "demo"))

    for svc in SERVICES:
        scope_spans = scope + b"".join(f_bytes(2, x) for x in spans[svc])
        post(a.url, "/v1/otlp/v1/traces", f_bytes(1, f_bytes(1, res(svc)) + f_bytes(2, scope_spans)),
             {"x-greptime-pipeline-name": "greptime_trace_v1", **db_header})
        scope_logs = scope + b"".join(f_bytes(2, x) for x in logs[svc])
        post(a.url, "/v1/otlp/v1/logs", f_bytes(1, f_bytes(1, res(svc)) + f_bytes(2, scope_logs)), db_header)
    print(f"seeded {a.traces} traces, {sum(map(len, spans.values()))} spans, {sum(map(len, logs.values()))} logs into {a.url} (database {a.db})")


main()
