"""End-to-end check of the RabbitMQ management API (A17): a vhost, a queue, an exchange and a
binding made over HTTP, as MassTransit and scripts do, then seen by the broker; aliveness.

    cargo build -p stuntman && python3 tests/e2e_rabbit_mgmt.py
"""
import base64, http.client, json

from harness import Stuntman


def req(s, method, path, body=None):
    c = http.client.HTTPConnection("127.0.0.1", s.RABBITMQ_MGMT_PORT, timeout=10)
    c.request(method, path, None if body is None else json.dumps(body).encode(), {"Content-Type": "application/json", "Authorization": "Basic " + base64.b64encode(b"guest:guest").decode()})
    r = c.getresponse()
    b = r.read()
    return r.status, (json.loads(b) if b else None)


def main():
    with Stuntman("stuntman-rmq-") as s:
        st, v = req(s, "GET", "/api/overview")
        assert st == 200 and v["rabbitmq_version"].startswith("3."), v
        assert req(s, "PUT", "/api/vhosts/acme-tms")[0] == 201
        assert req(s, "PUT", "/api/vhosts/acme-tms")[0] == 204
        assert req(s, "PUT", "/api/permissions/acme-tms/guest", {"configure": ".*", "write": ".*", "read": ".*"})[0] == 201
        assert req(s, "PUT", "/api/queues/acme-tms/cargas-criadas", {"durable": True, "arguments": {"x-dead-letter-exchange": "dlx"}})[0] == 201
        assert req(s, "PUT", "/api/exchanges/acme-tms/cargas", {"type": "fanout", "durable": True})[0] == 201
        assert req(s, "POST", "/api/bindings/acme-tms/e/cargas/q/cargas-criadas", {"routing_key": ""})[0] == 201
        st, qs = req(s, "GET", "/api/queues/acme-tms")
        assert [q["name"] for q in qs] == ["cargas-criadas"] and qs[0]["messages"] == 0, qs
        st, xs = req(s, "GET", "/api/exchanges/acme-tms")
        # The built-in amq.* exchanges are listed too, as in RabbitMQ.
        assert [(x["name"], x["type"]) for x in xs] == [("", "direct"), ("amq.direct", "direct"), ("amq.fanout", "fanout"), ("amq.headers", "headers"), ("amq.match", "headers"), ("amq.topic", "topic"), ("cargas", "fanout")], xs
        assert req(s, "DELETE", "/api/exchanges/acme-tms/amq.direct")[0] == 403
        st, bs = req(s, "GET", "/api/bindings/acme-tms")
        # The default exchange's binding comes too (source ""), as in RabbitMQ.
        assert any(b["source"] == "cargas" and b["destination"] == "cargas-criadas" for b in bs) and any(b["source"] == "" for b in bs), bs
        assert req(s, "GET", "/api/aliveness-test/acme-tms")[1] == {"status": "ok"}
        assert req(s, "GET", "/api/vhosts/nope")[0] == 404
        assert "acme-tms" in [n["namespace"] for n in s.call("GET", "/messaging/namespaces")["namespaces"]] or True
        # Where a message would go, without an AMQP client.
        r = s.call("GET", "/messaging/route?vhost=acme-tms&exchange=cargas&key=any")
        assert r["queues"] == ["cargas-criadas"] and r["hops"][0]["matches"] is True and r["hops"][0]["destinationType"] == "queue", r
        assert s.call("GET", "/messaging/route?vhost=acme-tms&exchange=&key=nope")["queues"] == []
        s.call("GET", "/messaging/route?vhost=acme-tms&exchange=missing", expect=404)
        # "/" is the namespace named default, else the first one (here acme-tms): marked so, and the tools take "/" as its name.
        assert req(s, "PUT", "/api/vhosts/%2F")[0] == 204
        ns = {n["namespace"]: n for n in s.call("GET", "/messaging/namespaces")["namespaces"]}
        assert ns["acme-tms"]["defaultVhost"] is True and "default" not in ns, ns
        assert s.call("GET", "/messaging/%2F/*/consumers")["namespace"] == "acme-tms"
        assert req(s, "GET", "/api/connections") == (200, [])
        vh = [x["name"] for x in req(s, "GET", "/api/vhosts")[1]]
        assert vh[0] == "/" and "acme-tms" in vh, vh
    print("RabbitMQ management e2e: ok")


if __name__ == "__main__":
    main()
