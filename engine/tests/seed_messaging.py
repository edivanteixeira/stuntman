"""Demo messaging state for the screens (M1–M3), made the way an app would: the official
azure-servicebus SDK publishes, receives, completes, abandons (to dead-letter) and schedules on
acme-bus-stg; acme-rabbit-stg gets its queues and messages through Stuntman's API.

    python3 tests/seed_messaging.py [http://localhost:8025] [5673]
"""
import datetime, json, sys, urllib.error, urllib.request
from azure.servicebus import ServiceBusClient, ServiceBusMessage

API = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"
PORT = sys.argv[2] if len(sys.argv) > 2 else "5673"


def call(method, path, body=None):
    req = urllib.request.Request(API + "/api" + path, method=method, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json", "X-Stuntman-Client": "seed"})
    try:
        return json.loads(urllib.request.urlopen(req).read() or b"null")
    except urllib.error.HTTPError as e:
        return {"status": e.code}


SB, RB = "azure-service-bus", "rabbitmq"
call("POST", f"/services/{SB}/accounts", {"account": "acme-bus-stg"})
for q, extra in [("orders-created", {"max_delivery": 10}), ("invoice-render", {}), ("email-outbox", {}), ("payments-retry", {"sessions": True})]:
    call("POST", f"/services/{SB}/accounts/acme-bus-stg/entities", {"entity": q, "kind": "queue", **extra})
call("POST", f"/services/{SB}/accounts/acme-bus-stg/entities", {"entity": "document-events", "kind": "topic"})
for s in ["audit", "webhooks", "search-index"]:
    call("POST", f"/services/{SB}/accounts/acme-bus-stg/topics/document-events/subscriptions", {"subscription": s})
call("POST", f"/services/{RB}/accounts", {"account": "acme-rabbit-stg"})
for q in ["notifications", "audit-log"]:
    call("POST", f"/services/{RB}/accounts/acme-rabbit-stg/entities", {"entity": q, "kind": "queue"})

CS = f"Endpoint=sb://acme-bus-stg.localhost:{PORT};SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;UseDevelopmentEmulator=true"
order = lambda n, c: json.dumps({"orderId": n, "customer": c, "total": 1290.0 if n % 2 else 75.0, "currency": "USD"})
with ServiceBusClient.from_connection_string(CS) as sb:
    with sb.get_queue_sender("orders-created") as s:
        s.send_messages([ServiceBusMessage(order(58200 + i, "emma.carter@example.com"), subject="OrderCreated", application_properties={"tenant": "acme"}, correlation_id=f"req-7f3c{i:02d}") for i in range(10)])
        s.send_messages(ServiceBusMessage(json.dumps({"orderId": 58190}), subject="OrderCancelled", application_properties={"tenant": "globex"}))
        s.schedule_messages(ServiceBusMessage(order(58300, "liam.walker@example.com"), subject="OrderCreated"), datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1))
    with sb.get_queue_receiver("orders-created", max_wait_time=3, prefetch_count=0) as r:
        ms = r.receive_messages(max_message_count=6, max_wait_time=3)
        for m in ms[:4]:
            r.complete_message(m)
        # One message the handler can never process: abandoned until it is dead-lettered.
        bad = ms[4]
        r.abandon_message(bad)
        for _ in range(9):
            again = r.receive_messages(max_message_count=10, max_wait_time=2)
            for m in again:
                if m.message_id == bad.message_id:
                    r.abandon_message(m)
                else:
                    r.abandon_message(m)
        r.dead_letter_message(ms[5], reason="BadOrder", error_description="total is negative") if False else None
    with sb.get_queue_sender("email-outbox") as s:
        s.send_messages([ServiceBusMessage(json.dumps({"to": f"user{i}@example.com", "template": "receipt"}), subject="SendEmail") for i in range(3)])
    with sb.get_topic_sender("document-events") as s:
        for i in range(7):
            s.send_messages(ServiceBusMessage(json.dumps({"document": f"nda-acme-{i}.pdf", "event": "signed"}), subject="DocumentSigned"))
    with sb.get_subscription_receiver("document-events", "audit", max_wait_time=3) as r:
        for m in r.receive_messages(max_message_count=7, max_wait_time=3):
            r.complete_message(m)
    with sb.get_subscription_receiver("document-events", "search-index", max_wait_time=3) as r:
        for m in r.receive_messages(max_message_count=7, max_wait_time=3):
            r.complete_message(m)

for i in range(4):
    call("POST", "/messaging/acme-rabbit-stg/notifications/messages", {"subject": "UserNotified", "body": {"user": f"ava.morgan{i}@example.com", "channel": "email"}})
call("PUT", "/messaging/acme-bus-stg/orders-created/next-delivery", {"behavior": "duplicate", "times": 1})
print("messaging seeded")
