"""End-to-end check of the ActiveMQ (OpenWire) broker with the official .NET client (Apache.NMS.ActiveMQ,
tests/activemq_client): user and password, messages seeded by the partner (activemq_send), the app's own sends,
Receive(timeout) until null with auto and client acks, prefetch 0 (MessagePull), a wrong password, the
provocations refuse_login, drop_after and fail_commits, local transactions (commit and rollback), a durable
subscriber that disconnects and comes back, selectors (and an invalid one), message groups, JMSPriority order,
scheduled delivery (delay, repeat, cron on the environment clock), expiration, a compressed body, noLocal and exclusive
consumers, dead letters (expired, past max_redeliveries, poisoned by the client, per-destination DLQ) consumed from
the DLQ, ActiveMQ.Scheduler.Management (BROWSE, REMOVE), consumer priority and advisories, all checked through the tools. XA, blob messages and the tight encoding are checked with the Java client.

    cargo build -p stuntman && python3 tests/e2e_activemq.py
"""
import os, shutil, subprocess, tempfile, time, urllib.parse

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
                return s.call("GET", "/activemq/messages?" + urllib.parse.urlencode({"destination": "queue://positions", **q}))["messages"]

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

            def dest(name):
                return [x for x in s.call("GET", "/activemq/destinations")["destinations"] if x["name"] == name][0]

            # Local transactions: a rolled back send, sends invisible until the commit, a rolled back receive redelivered.
            code, out = client("acme", "s3cret", "tx")
            check(code == 0, "transactions: the client's checks passed (invisible until commit, rollback redelivers)")
            tx = msgs(destination="queue://orders.tx")
            check(sorted((m["text"], m["state"]) for m in tx) == [("ORD-0", "rolled_back"), ("ORD-1", "acked"), ("ORD-2", "acked")], f"ORD-0 rolled back, ORD-1 and ORD-2 acked at the commit: {[(m['text'], m['state']) for m in tx]}")
            check([m["redeliveryCount"] for m in tx if m["text"] == "ORD-1"] == [1], "ORD-1 counts one redelivery (the rollback)")
            txs = s.call("GET", "/activemq/destinations")["transactions"]
            check(len(txs) >= 3 and all(t["kind"] == "local" and t["state"] in ("committed", "rolled_back") for t in txs), f"list_activemq_destinations lists the local transactions: {[(t['state'], t['sends'], t['acks']) for t in txs]}")
            s.call("PUT", "/activemq/settings", {"fail_commits": 1})
            code, out = client("acme", "s3cret", "txfail")
            check("commit: TransactionRolledBackException" in out, "fail_commits: the commit fails with a TransactionRolledBackException")
            check(msgs(destination="queue://orders.tx", last=1)[0]["state"] == "rolled_back" and s.call("GET", "/activemq/destinations")["settings"]["failCommits"] == 0, "and its send is rolled back")

            # Durable subscription: subscribe and leave, the partner publishes, come back for what it kept.
            code, out = client("acme", "s3cret", "durable")
            check(code == 0 and "consumed: 0" in out, "durable subscriber registered (clientId acme-dashboard, prices-sub)")
            for region, price in [("north", 10), ("south", 11), ("north", 12)]:
                s.call("POST", "/activemq/messages", dict(destination="topic://prices", text=f"PRICE-{price}", properties={"region": region}))
            sub = dest("topic://prices")["subscriptions"]
            check(len(sub) == 1 and sub[0]["name"] == "prices-sub" and sub[0]["clientId"] == "acme-dashboard" and sub[0]["pending"] == 2 and not sub[0]["active"] and sub[0]["selector"] == "region = 'north'",
                  f"offline, the subscription keeps the 2 north prices: {sub}")
            code, out = client("acme", "s3cret", "durable", "unsubscribe")
            check("consumed: 2" in out and "PRICE-10" in out and "PRICE-12" in out and "PRICE-11" not in out and "unsubscribed" in out, "back online it got both, then unsubscribed")
            check(dest("topic://prices")["subscriptions"] == [], "RemoveSubscriptionInfo deleted the subscription")

            # Selectors.
            for region, amount in [("north", 250), ("north", 50), ("south", 500)]:
                s.call("POST", "/activemq/messages", dict(destination="queue://orders.sel", text=f"{region}-{amount}", properties={"region": region, "amount": amount}))
            code, out = client("acme", "s3cret", "select")
            check("consumed: 1" in out and "north-250" in out, "the selector region = 'north' AND amount > 100 took only north-250")
            check("invalid selector: InvalidSelectorException" in out, "an invalid selector is refused with InvalidSelectorException: " + next((l for l in out.splitlines() if "invalid selector" in l), ""))
            check(sorted(m["state"] for m in msgs(destination="queue://orders.sel")) == ["acked", "pending", "pending"], "the other two stay pending")

            # Message groups: two consumers with prefetch 1; each group sticks to one of them.
            for i, g in enumerate("ABABAB"):
                s.call("POST", "/activemq/messages", dict(destination="queue://orders.grp", text=f"{g}-{i}", group_id=f"TRK-{g}"))
            code, out = client("acme", "s3cret", "groups", uri=uri + "?nms.PrefetchPolicy.QueuePrefetch=1")
            owners = {}
            for m in msgs(destination="queue://orders.grp"):
                owners.setdefault(m["headers"]["groupId"], set()).add(m["consumer"])
            check(len(owners) == 2 and all(len(v) == 1 for v in owners.values()) and owners["TRK-A"] != owners["TRK-B"], f"JMSXGroupID: each group went to one consumer, a different one each ({[len(v) for v in owners.values()]})")

            # JMSPriority order, scheduled delivery, expiration.
            s.call("POST", "/activemq/messages", dict(destination="queue://orders.prio", text="low", priority=1))
            s.call("POST", "/activemq/messages", dict(destination="queue://orders.prio", text="high", priority=9))
            later = s.call("POST", "/activemq/messages", dict(destination="queue://orders.prio", text="later", delay="2s"))["seqs"][0]
            s.call("POST", "/activemq/messages", dict(destination="queue://orders.prio", text="stale", ttl="300ms"))
            check(dest("queue://orders.prio")["counts"]["scheduled"] == 1, "AMQ_SCHEDULED_DELAY: the delayed message waits as scheduled")
            time.sleep(0.5)
            check([m["state"] for m in msgs(destination="queue://orders.prio") if m["text"] == "stale"] == ["expired"], "past its JMSExpiration the message is expired")
            code, out = client("acme", "s3cret", "drain", "queue://orders.prio")
            got = [l.split()[1] for l in out.splitlines() if l.startswith("got:")]
            check(got == ["high", "low", "later"], f"JMSPriority 9 before 1, the scheduled one when its time came, the expired one never: {got}")
            check(s.call("GET", f"/activemq/wait-ack?destination=queue://orders.prio&seq={later}&timeout=5s")["counts"]["expired"] == 1, "counts: acked 3, expired 1")

            # A compressed body (UseCompression) is inflated on arrival.
            code, out = client("acme", "s3cret", "produce", '{"order": "ORD-77", "note": "' + "acme " * 50 + '"}', uri=uri + "?nms.UseCompression=true")
            z = msgs(destination="queue://orders.z", last=1)
            check(code == 0 and z and z[0].get("text", "").startswith('{"order": "ORD-77"') and "compressed" not in z[0]["headers"], "a compressed TextMessage is kept as its plain text")
            code, out = client("acme", "s3cret", "drain", "queue://orders.z")
            check("consumed: 1" in out and "ORD-77" in out, "and dispatched plain to a consumer")

            # noLocal and exclusive consumers (failover when the exclusive one leaves).
            code, out = client("acme", "s3cret", "nolocal")
            check(code == 0, "noLocal: the subscriber's own alert is skipped, another connection's comes")
            check([m["state"] for m in msgs(destination="topic://alerts") if m["text"] == "own-1"] == ["discarded"], "the own alert had no taker: discarded")
            for i in range(4):
                s.call("POST", "/activemq/messages", dict(destination="queue://orders.ex", text=f"EX-{i}"))
            code, out = client("acme", "s3cret", "exclusive", uri=uri + "?nms.PrefetchPolicy.QueuePrefetch=1")
            check(code == 0 and "exclusive: c1=2 c2=2" in out, "exclusive: the first consumer took the queue, the second took over when it closed")

            # Scheduler: a repeat every 500 ms, and a cron entry on the environment clock.
            s.call("POST", "/activemq/messages", dict(destination="queue://orders.rep", text="tick", repeat=2, period="500ms"))
            time.sleep(1.6)
            check(sorted(m["state"] for m in msgs(destination="queue://orders.rep")) == ["pending"] * 3, "AMQ_SCHEDULED_REPEAT 2: three deliveries, 500 ms apart")
            nightly = s.call("POST", "/activemq/messages", dict(destination="queue://orders.cron", text="nightly", cron="0 2 * * *"))["seqs"][0]
            job = msgs(destination="queue://orders.cron")[0]
            check(job["state"] == "scheduled" and job["schedule"]["cron"] == "0 2 * * *" and job["deliverAt"] % 86_400_000 == 2 * 3_600_000, f"AMQ_SCHEDULED_CRON: scheduled for 02:00 UTC ({job.get('deliverAt')})")
            s.call("POST", "/clock/advance", {"by": "1d"})
            cron = msgs(destination="queue://orders.cron")
            check(sorted(m["state"] for m in cron) == ["pending", "scheduled"] and [m["deliverAt"] for m in cron if m["seq"] == nightly] == [job["deliverAt"] + 86_400_000],
                  "a day later on the environment clock: one delivery, the next one tomorrow at 02:00")
            s.call("POST", "/clock/advance", {"reset": True})
            check(s.call("POST", "/activemq/scheduled/cancel", {"seq": nightly})["cancelled"] == 1, "cancel_activemq_scheduled stops the cron entry")

            # Dead letters, ActiveMQ's way: expired (persistent) and poisoned messages move to ActiveMQ.DLQ.
            stale = [m for m in msgs(destination="queue://orders.prio") if m["text"] == "stale"][0]
            check(stale["movedTo"]["destination"] == "queue://ActiveMQ.DLQ", f"the expired message points at its copy in ActiveMQ.DLQ: {stale.get('movedTo')}")
            s.call("PUT", "/activemq/settings", {"max_redeliveries": 1, "redeliver": 5})
            s.call("POST", "/activemq/messages", dict(destination="queue://orders.dlq", text="poison-1"))
            code, out = client("acme", "s3cret", "drain", "queue://orders.dlq")
            s.call("PUT", "/activemq/settings", {"max_redeliveries": 6, "redeliver": 0})
            p1 = msgs(destination="queue://orders.dlq")[0]
            check("consumed: 2" in out and p1["state"] == "dead_letter" and p1["movedTo"]["destination"] == "queue://ActiveMQ.DLQ", f"max_redeliveries 1: after the second delivery it went to the DLQ ({p1['state']}, {p1.get('movedTo')})")
            s.call("POST", "/activemq/messages", dict(destination="queue://orders.poison", text="bad-1"))
            code, out = client("acme", "s3cret", "poison")
            b1 = msgs(destination="queue://orders.poison")[0]
            check("poison: received 7 times" in out and b1["state"] == "dead_letter" and "movedTo" in b1, f"the client's redelivery policy (6) sent a poison ack: to the DLQ ({out.strip().splitlines()[-1]})")
            s.call("PUT", "/activemq/settings", {"individual_dlq": True})
            s.call("POST", "/activemq/messages", dict(destination="queue://orders.ind", text="late", ttl="200ms"))
            time.sleep(0.4)
            check(msgs(destination="queue://orders.ind")[0]["movedTo"]["destination"] == "queue://ActiveMQ.DLQ.Queue.orders.ind", "individual_dlq: ActiveMQ.DLQ.Queue.orders.ind")
            s.call("PUT", "/activemq/settings", {"individual_dlq": False})
            dlq = msgs(destination="queue://ActiveMQ.DLQ")
            check(sorted((m["text"], m["headers"]["originalDestination"]) for m in dlq) == [("bad-1", "queue://orders.poison"), ("poison-1", "queue://orders.dlq"), ("stale", "queue://orders.prio")]
                  and all(m["properties"].get("dlqDeliveryFailureCause") for m in dlq), "ActiveMQ.DLQ holds the three, with their original destination and failure cause")
            code, out = client("acme", "s3cret", "drain", "queue://ActiveMQ.DLQ")
            check("consumed: 3" in out, "and the app consumes them from ActiveMQ.DLQ")

            # ActiveMQ.Scheduler.Management over JMS, consumer priority, advisories.
            for h in (1, 2):
                s.call("POST", "/activemq/messages", dict(destination="queue://orders.later", text=f"in {h}h", delay=f"{h}h"))
            code, out = client("acme", "s3cret", "sched")
            check("browse: 2" in out and "after remove: 1" in out, "Scheduler.Management: BROWSE sent both jobs to the JMSReplyTo, REMOVE took one away")
            check(sorted(m["state"] for m in msgs(destination="queue://orders.later")) == ["discarded", "scheduled"], "the removed job is discarded")
            code, out = client("acme", "s3cret", "priority", uri=uri + "?nms.PrefetchPolicy.QueuePrefetch=2")
            check("priority: high=P-0 P-1 low=P-2 P-3" in out, "consumer.priority 9 gets messages first while it has prefetch room")
            s.call("PUT", "/activemq/settings", {"advisory_delivered": True})
            code, out = client("acme", "s3cret", "advisory")
            s.call("PUT", "/activemq/settings", {"advisory_delivered": False})
            check("advisory consumer: Advisory:ConsumerInfo" in out and "advisory delivered: Advisory:ActiveMQTextMessage" in out, "advisories reach NMS with their data structures (ConsumerInfo, the delivered message)")
            check(all("ActiveMQ.Advisory" not in d["name"] for d in s.call("GET", "/activemq/destinations")["destinations"]), "advisory topics stay out of the destination list")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print("ActiveMQ e2e: " + ("ok" if not failed else f"{failed} failed"))
    raise SystemExit(1 if failed else 0)


if __name__ == "__main__":
    main()
