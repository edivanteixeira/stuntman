"""End-to-end check of the ActiveMQ (OpenWire) broker with the official .NET client (Apache.NMS.ActiveMQ,
tests/activemq_client): user and password, messages seeded by the partner (activemq_send), the app's own sends,
Receive(timeout) until null with auto and client acks, prefetch 0 (MessagePull), a wrong password, and the
provocations refuse_login and drop_after, all checked through the tools.

    cargo build -p stuntman && python3 tests/e2e_activemq.py
"""
import os, shutil, subprocess, tempfile, urllib.parse

from harness import Stuntman

HERE = os.path.dirname(os.path.abspath(__file__))
failed = 0


def check(ok, what):
    global failed
    print(("ok   " if ok else "FAIL ") + what)
    failed += 0 if ok else 1


def build():
    # Built in a copy: bin/ and obj/ stay out of the tree.
    d = tempfile.mkdtemp(prefix="stuntman-amq-client-")
    shutil.copytree(os.path.join(HERE, "activemq_client"), os.path.join(d, "src"))
    subprocess.run(["dotnet", "build", "-nologo", "-v", "q", "-o", os.path.join(d, "out"), os.path.join(d, "src")], check=True, stdout=subprocess.DEVNULL)
    return d, os.path.join(d, "out", "activemq_client.dll")


def main():
    work, dll = build()
    try:
        with Stuntman("stuntman-amq-") as s:
            uri = f"activemq:tcp://127.0.0.1:{s.ACTIVEMQ_PORT}"

            def client(*args, uri=uri):
                r = subprocess.run(["dotnet", dll, uri, *args], capture_output=True, text=True, timeout=120)
                print("  | " + r.stdout.strip().replace("\n", "\n  | "))
                return r.returncode, r.stdout

            def msgs(**q):
                return s.call("GET", "/activemq/messages?" + urllib.parse.urlencode(dict(destination="queue://positions", **q)))["messages"]

            def send(text, **kw):
                return s.call("POST", "/activemq/messages", dict(destination="queue://positions", text=text, **kw))

            s.call("PUT", "/activemq/settings", {"users": {"acme": "s3cret"}})
            send('{"truck":"TRK-001"}', properties={"region": "south"}, correlation_id="partner-1")
            send('{"truck":"TRK-002"}', properties={"region": "south"})

            code, out = client("acme", "s3cret", "run")
            check(code == 0, "the .NET client ran its checks (produce 3, consume, wrong password)")
            check("consumed: 5" in out, "Receive(timeout) until null got the 2 seeded and the 3 produced messages")
            check("TRK-001\"} region=south" in out, "a seeded message came with its property")
            done = s.call("GET", "/activemq/wait-ack?destination=queue://positions&timeout=10s")
            check(done["counts"]["acked"] == 5 and done["counts"]["pending"] == 0 and done["counts"]["delivered"] == 0, f"all 5 acked: {done['counts']}")
            own = [m for m in msgs(state="acked") if m["via"] == "openwire"]
            check(len(own) == 3 and all(m["sender"] == "acme" and m["kind"] == "text" for m in own), "the app's 3 sends are kept as sent by acme over OpenWire")
            check({m["headers"].get("correlationId") for m in own} == {"acme-TRK-100", "acme-TRK-200", "acme-TRK-300"} and own[0]["properties"] == {"region": "north"}, "with their correlation id and properties")

            # Prefetch 0: every Receive sends a MessagePull; the empty answer ends the loop on time. Client acknowledge.
            seqs = send("pull-me", count=2)["seqs"]
            code, out = client("acme", "s3cret", "consume", "client", uri=uri + "?nms.PrefetchPolicy.QueuePrefetch=0")
            check(code == 0 and "consumed: 2" in out, "prefetch 0 (MessagePull) with client acknowledge got 2")
            check(s.call("GET", f"/activemq/wait-ack?destination=positions&seq={seqs[1]}&timeout=10s")["counts"]["acked"] == 7, "and acked them")

            s.call("PUT", "/activemq/settings", {"refuse_login": True})
            code, out = client("acme", "s3cret", "login")
            check("login: security:" in out, "refuse_login: even the right password gets a security exception")
            s.call("PUT", "/activemq/settings", {"refuse_login": False})

            s.call("PUT", "/activemq/settings", {"drop_after": 1})
            send("drop-1"), send("drop-2")
            code, out = client("acme", "s3cret", "consume")
            check("error:" in out, "drop_after 1: the connection closes after the first dispatch")
            left = msgs(last=2)
            check([m["state"] for m in left] == ["pending", "pending"] and left[1]["redeliveryCount"] == 1, f"what it held is pending again, redelivered once: {[(m['state'], m['redeliveryCount']) for m in left]}")
            s.call("PUT", "/activemq/settings", {"drop_after": 0})
            code, out = client("acme", "s3cret", "consume")
            check("consumed: 2" in out and "drop-1 region= redelivered=True" in out, "the next connection gets both, the first marked redelivered")

            d = s.call("GET", "/activemq/destinations")
            q = [x for x in d["destinations"] if x["name"] == "queue://positions"][0]
            check(q["counts"]["acked"] == 9 and q["consumers"] == [] and d["settings"]["users"] == ["acme"], f"list_activemq_destinations: {q['counts']}")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print("ActiveMQ e2e: " + ("ok" if not failed else f"{failed} failed"))
    raise SystemExit(1 if failed else 0)


if __name__ == "__main__":
    main()
