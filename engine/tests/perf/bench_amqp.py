"""Message timings of a running Stuntman over AMQP 0-9-1 (RabbitMQ clients): publish with a
confirm, publish to delivery, and consume with an ack. Persistent messages on a durable queue.

    pip install pika && python3 tests/perf/bench_amqp.py 127.0.0.1 5673 15672 [--n 3000]
"""
import base64, json, sys, threading, time, urllib.request

import pika

HOST, AMQP, MGMT = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
N = int(sys.argv[sys.argv.index("--n") + 1]) if "--n" in sys.argv else 3000
VH, Q = "bench", "bench-q"


def mgmt(method, path, body=None):
    r = urllib.request.Request(f"http://{HOST}:{MGMT}/api{path}", method=method, data=json.dumps(body or {}).encode(),
                               headers={"Content-Type": "application/json", "Authorization": "Basic " + base64.b64encode(b"guest:guest").decode()})
    urllib.request.urlopen(r, timeout=30).read()


def conn():
    return pika.BlockingConnection(pika.ConnectionParameters(HOST, AMQP, VH, pika.PlainCredentials("guest", "guest")))


def p50(xs):
    return sorted(xs)[len(xs) // 2] * 1000


def main():
    mgmt("PUT", f"/vhosts/{VH}")
    ch = conn().channel()
    ch.queue_declare(Q, durable=True)
    ch.confirm_delivery()
    persistent = pika.BasicProperties(delivery_mode=2)

    # Publish, each waiting for its confirm.
    lat, t0 = [], time.perf_counter()
    for i in range(N):
        t = time.perf_counter()
        ch.basic_publish("", Q, b"x" * 200, persistent)
        lat.append(time.perf_counter() - t)
    pub_rate = N / (time.perf_counter() - t0)
    print(f"publish (confirmed)     {pub_rate:8.0f} msg/s   p50 {p50(lat):6.2f} ms", flush=True)

    # Consume what is there, acking each.
    c2 = conn().channel()
    c2.basic_qos(prefetch_count=100)
    got, t0 = [0], time.perf_counter()

    def on(c, m, p, b):
        c.basic_ack(m.delivery_tag)
        got[0] += 1
        if got[0] == N:
            c.stop_consuming()
    c2.basic_consume(Q, on)
    c2.start_consuming()
    print(f"consume (acked)         {N / (time.perf_counter() - t0):8.0f} msg/s", flush=True)

    # Publish to delivery: one message at a time, a consumer waiting on another connection.
    seen, done = threading.Event(), []

    def consumer():
        c = conn().channel()
        def on2(c, m, p, b):
            c.basic_ack(m.delivery_tag)
            done.append(time.perf_counter() - float(b))
            seen.set()
            if len(done) == 500:
                c.stop_consuming()
        c.basic_consume(Q, on2)
        c.start_consuming()
    th = threading.Thread(target=consumer, daemon=True)
    th.start()
    time.sleep(0.5)
    for _ in range(500):
        seen.clear()
        ch.basic_publish("", Q, repr(time.perf_counter()).encode(), persistent)
        seen.wait(5)
    print(f"publish to delivery     p50 {p50(done):6.2f} ms", flush=True)


if __name__ == "__main__":
    main()
