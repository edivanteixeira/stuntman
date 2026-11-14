"""Stuntman's Service Bus side, checked with the official Python SDK (azure-servicebus, its own
AMQP 1.0 stack): management (create queue, topic, subscription), send, peek-lock receive,
complete, abandon, dead-letter, scheduled send, renew lock, peek, topic fan-out.

    python3 tests/e2e_servicebus.py [http://localhost:8025] [5673]
"""
import datetime, json, sys, time, urllib.error, urllib.request
from azure.servicebus import ServiceBusClient, ServiceBusMessage, ServiceBusSubQueue

API = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"
PORT = sys.argv[2] if len(sys.argv) > 2 else "5673"
NS = "sb-check"
CS = f"Endpoint=sb://{NS}.localhost:{PORT};SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;UseDevelopmentEmulator=true"
failed = 0


def check(ok, what):
    global failed
    print(("ok   " if ok else "FAIL ") + what)
    failed += 0 if ok else 1


def api(path):
    return json.loads(urllib.request.urlopen(API + "/api" + path).read())


def call(method, path, body=None):
    req = urllib.request.Request(API + "/api" + path, method=method, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json", "X-Stuntman-Client": "e2e-servicebus"})
    try:
        return json.loads(urllib.request.urlopen(req).read() or b"null")
    except urllib.error.HTTPError as e:
        return {"status": e.code, "error": e.read().decode()}


# The Python admin client insists on HTTPS; the namespace and its entities are made through
# Stuntman's API here (the .NET test covers ServiceBusAdministrationClient over HTTP).
call("DELETE", f"/services/azure-service-bus/accounts/{NS}")
call("POST", "/services/azure-service-bus/accounts", {"account": NS})
call("POST", f"/services/azure-service-bus/accounts/{NS}/entities", {"entity": "orders", "kind": "queue", "max_delivery": 3})
call("POST", f"/services/azure-service-bus/accounts/{NS}/entities", {"entity": "events", "kind": "topic"})
r = call("POST", f"/services/azure-service-bus/accounts/{NS}/topics/events/subscriptions", {"subscription": "audit", "max_delivery": 5})
check(any(e["name"] == "events" and e["subscriptions"] for e in r.get("entities", [])), "namespace, queue, topic and subscription made through the API")

with ServiceBusClient.from_connection_string(CS) as sb:
    with sb.get_queue_sender("orders") as s:
        s.send_messages([ServiceBusMessage(json.dumps({"orderId": i}), subject="OrderCreated", application_properties={"tenant": "globex"}, message_id=f"m-{i}") for i in range(3)])
    check(True, "send 3 messages")
    with sb.get_queue_receiver("orders", max_wait_time=5) as r:
        ms = r.receive_messages(max_message_count=3, max_wait_time=5)
        check(len(ms) == 3 and ms[0].subject == "OrderCreated" and ms[0].application_properties.get(b"tenant", ms[0].application_properties.get("tenant")) in (b"globex", "globex"), "peek-lock receive: subject and application properties")
        check(ms[0].sequence_number > 0 and ms[0].locked_until_utc is not None and str(ms[0].lock_token) != "", "sequence number, locked-until and lock token")
        before = ms[0].locked_until_utc
        time.sleep(1.1)
        r.renew_message_lock(ms[0])
        check(ms[0].locked_until_utc > before, "renew lock (management)")
        r.complete_message(ms[0])
        r.abandon_message(ms[1])
        r.dead_letter_message(ms[2], reason="BadOrder", error_description="total is negative")
        check(True, "complete, abandon, dead-letter")
        again = r.receive_messages(max_message_count=1, max_wait_time=5)
        check(len(again) == 1 and again[0].delivery_count == 1 and again[0].message_id == "m-1", "abandoned message comes back with delivery count 1")
        r.complete_message(again[0])
    with sb.get_queue_receiver("orders", sub_queue=ServiceBusSubQueue.DEAD_LETTER, max_wait_time=3) as d:
        peeked = d.peek_messages(max_message_count=5)
        check(len(peeked) == 1 and peeked[0].message_id == "m-2", "peek the dead-letter queue")
    with sb.get_queue_sender("orders") as s:
        seq = s.schedule_messages(ServiceBusMessage("later"), datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=2))
        check(len(seq) == 1, "schedule a message (management)")
    with sb.get_queue_receiver("orders", max_wait_time=8) as r:
        later = r.receive_messages(max_message_count=1, max_wait_time=8)
        check(len(later) == 1 and str(later[0]) == "later", "the scheduled message arrives after its time")
        if later: r.complete_message(later[0])
    with sb.get_topic_sender("events") as s:
        s.send_messages(ServiceBusMessage("fan-out"))
    with sb.get_subscription_receiver("events", "audit", max_wait_time=5) as r:
        got = r.receive_messages(max_message_count=1, max_wait_time=5)
        check(len(got) == 1 and str(got[0]) == "fan-out", "topic → subscription")
        if got: r.complete_message(got[0])

call("POST", f"/services/azure-service-bus/accounts/{NS}/entities", {"entity": "payments", "kind": "queue", "sessions": True})
from azure.servicebus import NEXT_AVAILABLE_SESSION
with ServiceBusClient.from_connection_string(CS) as sb:
    with sb.get_queue_sender("payments") as s:
        s.send_messages([ServiceBusMessage(f"a{i}", session_id="acct-1") for i in range(2)] + [ServiceBusMessage("b0", session_id="acct-2")])
    with sb.get_queue_receiver("payments", session_id="acct-1", max_wait_time=5) as r:
        got = r.receive_messages(max_message_count=5, max_wait_time=5)
        check([str(m) for m in got] == ["a0", "a1"] and all(m.session_id == "acct-1" for m in got), "session receiver gets only its session, in order")
        r.session.set_state(b"step-2")
        check(r.session.get_state() == b"step-2", "session state set and read")
        before = r.session.locked_until_utc
        time.sleep(1.1)
        r.session.renew_lock()
        check(r.session.locked_until_utc > before, "session lock renewed")
        for m in got: r.complete_message(m)
    with sb.get_queue_receiver("payments", session_id=NEXT_AVAILABLE_SESSION, max_wait_time=5) as r:
        check(r.session.session_id == "acct-2", "next available session is the one with messages")
        for m in r.receive_messages(max_message_count=5, max_wait_time=3): r.complete_message(m)
    # Dead-letter: read it like the app would, then complete it.
    with sb.get_queue_receiver("orders", sub_queue=ServiceBusSubQueue.DEAD_LETTER, max_wait_time=5) as d:
        dead = d.receive_messages(max_message_count=5, max_wait_time=5)
        check(len(dead) == 1 and dead[0].dead_letter_reason == "BadOrder", "receive from the dead-letter queue")
        for m in dead: d.complete_message(m)
    # Defer, then fetch by sequence number.
    with sb.get_queue_sender("orders") as s:
        s.send_messages(ServiceBusMessage("defer-me"))
    with sb.get_queue_receiver("orders", max_wait_time=5) as r:
        m = r.receive_messages(max_message_count=1, max_wait_time=5)[0]
        seq = m.sequence_number
        r.defer_message(m)
        back = r.receive_deferred_messages([seq])
        check(len(back) == 1 and str(back[0]) == "defer-me", "deferred message received by sequence number")
        r.complete_message(back[0])

c = api(f"/messaging/{NS}/orders/messages?last=20")["counts"]
check(c["completed"] == 6 and c["dead_letter"] == 0, f"Stuntman saw 6 completed and the dead-letter emptied ({c})")
dl = [m for m in api(f"/messaging/{NS}/orders/messages?state=completed")["messages"] if m["deadLetterReason"] == "BadOrder"]
check(len(dl) == 1 and dl[0]["deadLetterDescription"] == "total is negative", "dead-letter reason and description kept")
print("e2e servicebus: all checks passed" if failed == 0 else f"e2e servicebus: {failed} failed")
sys.exit(1 if failed else 0)
