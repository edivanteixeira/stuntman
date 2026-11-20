"""Write throughput of a running Stuntman: HTTP calls without and with a route, 1 and 16 clients.
Every call is recorded, so this measures the cost of a write per request.

    python3 tests/perf/bench_writes.py http://127.0.0.1:8025 http://127.0.0.1:8028 [--min 500]

--min N fails when one client without a route does fewer than N req/s. To see a slow disk on a
fast machine, run Stuntman on Linux with LD_PRELOAD=slow_fsync.so (see run_slow_disk.sh).
"""
import http.client, json, sys, threading, time, urllib.parse, urllib.request

YAML = """service:
  name: bench
  title: Bench
routes:
  - route: GET /ping
    decide:
      - answer: ok
  - route: GET /route/v1/driving/{coords}
    decide:
      - answer: route
answers:
  - name: ok
    status: 200
    body: '{"ok": true}'
  - name: route
    status: 200
    template: handlebars
    headers:
      Content-Type: application/json
      X-Request-Id: "{{randomValue length=12 type='ALPHANUMERIC'}}"
    body: |
      {"code": "Ok", "coordinates": "{{request.pathSegments.[3]}}", "waypoints": [{{#each request.query.stop}}{{#if @index}}, {{/if}}{"name": "{{this}}", "hint": "{{upper this}}"}{{/each}}],
       "routes": [{"distance": {{randomInt lower=1000 upper=90000}}, "duration": {{randomInt lower=60 upper=7200}}, "legs": [],
       "weight_name": "routability", "geometry": "{{request.query.geometries}}", "at": "{{now format='yyyy-MM-dd HH:mm:ss'}}"}]}
"""


def put_plugin(api):
    r = urllib.request.Request(f"{api}/api/plugins/bench", method="PUT", data=json.dumps({"yaml": YAML}).encode(), headers={"Content-Type": "application/json"})
    urllib.request.urlopen(r, timeout=30).read()


def run(apis, path, clients, seconds=5.0):
    u = urllib.parse.urlparse(apis)
    done, lat, stop = [0] * clients, [[] for _ in range(clients)], time.time() + seconds

    def worker(i):
        c = http.client.HTTPConnection(u.hostname, u.port, timeout=30)
        while time.time() < stop:
            t = time.perf_counter()
            c.request("GET", path)
            c.getresponse().read()
            lat[i].append(time.perf_counter() - t)
            done[i] += 1

    ts = [threading.Thread(target=worker, args=(i,)) for i in range(clients)]
    t0 = time.time()
    [t.start() for t in ts]
    [t.join() for t in ts]
    all_lat = sorted(x for l in lat for x in l)
    rate = sum(done) / (time.time() - t0)
    p50 = all_lat[len(all_lat) // 2] * 1000 if all_lat else 0
    return rate, p50


def main():
    api, apis = sys.argv[1], sys.argv[2]
    floor = float(sys.argv[sys.argv.index("--min") + 1]) if "--min" in sys.argv else 0
    put_plugin(api)
    rows = []
    for name, path in [("HTTP without a route", "/bench-none/x"), ("HTTP with a route", "/bench/ping")]:
        for n in (1, 16):
            rate, p50 = run(apis, path, n)
            rows.append((name, n, rate, p50))
            print(f"{name:22s} {n:2d} client(s): {rate:8.0f} req/s   p50 {p50:6.2f} ms", flush=True)
    one = rows[0][2]
    assert one >= floor, f"one client without a route: {one:.0f} req/s, below {floor:.0f}"


if __name__ == "__main__":
    main()
