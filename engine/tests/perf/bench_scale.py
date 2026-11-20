"""Per-call cost with a big data folder: 85 services and 300,000 calls in the history.
A call must cost about the same as on an empty folder.

    python3 tests/perf/bench_scale.py http://127.0.0.1:8025 http://127.0.0.1:8028 <data folder> [--max-ms 1] [--empty] [--services 85] [--calls 300000]

It adds the services through the API, writes the history straight into the environment's
duble.sqlite (much faster than 300,000 calls), then measures one client without and with a
route, and two read calls. --empty measures the folder as it is, for a baseline. --max-ms N fails when the p50 of a call without a route is above N ms.
"""
import glob, json, os, sqlite3, sys, time, urllib.request

from bench_writes import put_plugin, run



def flag(name, default):
    return int(sys.argv[sys.argv.index(name) + 1]) if name in sys.argv else default


SERVICES, CALLS = flag("--services", 85), flag("--calls", 300_000)


def plugin_yaml(i):
    routes = "".join(f"  - route: GET /v1/thing{j}/{{id}}\n    decide:\n      - answer: ok\n" for j in range(3))
    return f"service:\n  name: svc{i:03d}\n  title: Service {i}\n  real_host: svc{i:03d}.example.com\nroutes:\n{routes}answers:\n  - name: ok\n    status: 200\n    body: '{{\"ok\": true}}'\n"


def call(api, method, path, body=None):
    r = urllib.request.Request(f"{api}/api{path}", method=method, data=json.dumps(body).encode() if body is not None else None, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(r, timeout=60) as x:
        return json.loads(x.read() or b"null")


def seed_history(data):
    db = sorted(glob.glob(os.path.join(data, "**", "duble.sqlite"), recursive=True), key=os.path.getsize)[-1]
    cn = sqlite3.connect(db, timeout=30)
    body = json.dumps({"method": "GET", "path": "/v1/thing0/1", "headers": {"host": "svc001.example.com", "user-agent": "bench"}, "body": "x" * 100})
    now = int(time.time() * 1000)
    cn.execute("""WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < ?1)
        INSERT INTO api_calls(id, at, service, route, method, path, status, answer, sentence, request, response)
        SELECT 'seed' || i, ?2 - i * 10, printf('svc%03d', i % ?3), 'GET /v1/thing0/{id}', 'GET', '/v1/thing0/1', 200, 'ok', 'seeded', ?4, '{"status":200}' FROM n""",
               (CALLS, now, SERVICES, body))
    cn.execute("""WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < ?1)
        INSERT INTO trips(id, module, label, status, at, link, host) SELECT 'seed' || i, 'apis', 'seeded', 'ok', ?2 - i * 10, '', 'svc001' FROM n""", (CALLS, now))
    cn.commit()
    cn.close()
    return db


def main():
    api, apis, data = sys.argv[1], sys.argv[2], sys.argv[3]
    ceiling = float(sys.argv[sys.argv.index("--max-ms") + 1]) if "--max-ms" in sys.argv else 0
    put_plugin(api)
    if "--empty" not in sys.argv:
        for i in range(SERVICES):
            call(api, "PUT", f"/plugins/svc{i:03d}", {"yaml": plugin_yaml(i)})
        db = seed_history(data)
        print(f"{SERVICES} services, {CALLS} calls in {db} ({os.path.getsize(db) >> 20} MB)", flush=True)
    rows = []
    for name, path in [("HTTP without a route", "/bench-none/x"), ("HTTP with a route", "/bench/ping"), ("read: one call", "/api/apis/calls?limit=1"), ("read: services", "/api/apis/services")]:
        rate, p50 = run(apis if path[1:4] != "api" else api, path, 1, 3.0)
        rows.append((name, p50))
        print(f"{name:22s} 1 client: {rate:8.0f} req/s   p50 {p50:6.2f} ms", flush=True)
    assert not ceiling or rows[0][1] <= ceiling, f"a call without a route: p50 {rows[0][1]:.2f} ms, above {ceiling} ms"


if __name__ == "__main__":
    main()
