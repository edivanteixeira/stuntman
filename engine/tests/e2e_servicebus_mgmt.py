"""The Service Bus management port and the Test CA downloads, on a Stuntman started on free ports.

- management port (STUNTMAN_SERVICEBUS_MGMT_PORT, 5300 like the official emulator): plain HTTP
  (what .NET and JS send in emulator mode), HTTPS (Python, Java) and AMQP on the same port;
- the Python ServiceBusAdministrationClient creates a queue, a topic and a subscription there
  and updates them (If-Match: *),
  trusting the Test CA through REQUESTS_CA_BUNDLE, then the same connection string sends/receives;
- the .NET ServiceBusAdministrationClient console (tests/servicebus_admin_dotnet), if dotnet exists;
- /api/test-ca.{pem,crt,der,p12,pfx} parse with `cryptography`; keytool loads the p12 if Java exists.

    python3 tests/e2e_servicebus_mgmt.py
"""
import http.client, os, shutil, subprocess, sys, tempfile, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from harness import Stuntman  # noqa: E402

from cryptography import x509
from cryptography.hazmat.primitives.serialization import pkcs12

failed = 0


def check(ok, what):
    global failed
    print(("ok   " if ok else "FAIL ") + what)
    failed += 0 if ok else 1


with Stuntman() as s:
    api = f"http://127.0.0.1:{s.PORT}/api"
    get = lambda p: urllib.request.urlopen(api + p).read()  # noqa: E731

    # The Test CA in every format.
    pem = get("/test-ca.pem")
    ca = x509.load_pem_x509_certificate(pem)
    for ext in ("crt", "der"):
        check(x509.load_der_x509_certificate(get(f"/test-ca.{ext}")) == ca, f"/test-ca.{ext} is the CA in DER")
    for ext in ("p12", "pfx"):
        key, cert, extra = pkcs12.load_key_and_certificates(get(f"/test-ca.{ext}"), b"changeit")
        check(key is None and ca in ([cert] if cert else []) + list(extra), f"/test-ca.{ext} holds the CA and no private key (password changeit)")
    info = s.call("GET", "/apis/test-ca")
    check(info["urls"]["p12"].endswith("/api/test-ca.p12") and "java" in info["runtimes"] and "ask" in info["rule"], "get_test_ca gives URLs, per-runtime options and the ask-first rule")
    tmp = tempfile.mkdtemp(prefix="stuntman-ca-")
    ca_file = os.path.join(tmp, "stuntman-test-ca.pem")
    open(ca_file, "wb").write(pem)
    if shutil.which("keytool") and subprocess.run(["java", "-version"], capture_output=True).returncode == 0:
        p12 = os.path.join(tmp, "stuntman-test-ca.p12")
        open(p12, "wb").write(get("/test-ca.p12"))
        out = subprocess.run(["keytool", "-list", "-keystore", p12, "-storetype", "PKCS12", "-storepass", "changeit"], capture_output=True, text=True)
        check(out.returncode == 0 and "trustedCertEntry" in out.stdout, "keytool reads the p12 as a truststore (trustedCertEntry)")
    else:
        print("skip keytool: no Java")

    # The management port answers plain HTTP (the .NET and JS admin clients in emulator mode).
    ns = "sb-admin"
    s.call("DELETE", f"/services/azure-service-bus/accounts/{ns}", expect=None)
    s.call("POST", "/services/azure-service-bus/accounts", {"account": ns})
    c = http.client.HTTPConnection("127.0.0.1", s.SERVICEBUS_MGMT_PORT, timeout=10)
    c.request("GET", "/$Resources/queues?api-version=2021-05", headers={"Host": f"{ns}.localhost:{s.SERVICEBUS_MGMT_PORT}"})
    r = c.getresponse()
    check(r.status == 200 and b"<feed" in r.read(), "management port: plain HTTP lists queues")
    view = s.call("GET", f"/services/azure-service-bus/accounts/{ns}")
    check(f":{s.SERVICEBUS_MGMT_PORT};" in (view.get("managementConnection") or ""), "the namespace shows a management connection string with the port")

    # Python: HTTPS on the same port, then AMQP on it with the same connection string.
    os.environ["REQUESTS_CA_BUNDLE"] = ca_file
    from azure.servicebus import ServiceBusClient, ServiceBusMessage
    from azure.servicebus.management import ServiceBusAdministrationClient
    cs = f"Endpoint=sb://{ns}.localhost:{s.SERVICEBUS_MGMT_PORT};SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;UseDevelopmentEmulator=true"
    adm = ServiceBusAdministrationClient.from_connection_string(cs)
    adm.create_queue("payments-retry", max_delivery_count=4)
    adm.create_topic("order-placed")
    adm.create_subscription("order-placed", "warehouse")
    check(adm.get_queue("payments-retry").max_delivery_count == 4, "python admin: create_queue over HTTPS on the management port")
    check([x.name for x in adm.list_subscriptions("order-placed")] == ["warehouse"], "python admin: create_topic + create_subscription")
    # Update*: a PUT with If-Match: * (create again without it is 409).
    from datetime import timedelta
    from azure.core.exceptions import ResourceExistsError
    try:
        adm.create_queue("payments-retry")
        check(False, "python admin: create_queue on an existing queue is 409")
    except ResourceExistsError:
        check(True, "python admin: create_queue on an existing queue is 409")
    q = adm.get_queue("payments-retry")
    q.max_delivery_count, q.lock_duration = 7, timedelta(minutes=2)
    adm.update_queue(q)
    q = adm.get_queue("payments-retry")
    check((q.max_delivery_count, q.lock_duration) == (7, timedelta(minutes=2)), "python admin: update_queue changes max delivery count and lock")
    adm.update_topic(adm.get_topic("order-placed"))
    sub = adm.get_subscription("order-placed", "warehouse")
    sub.max_delivery_count = 3
    adm.update_subscription("order-placed", sub)
    check(adm.get_subscription("order-placed", "warehouse").max_delivery_count == 3 and [x.name for x in adm.list_subscriptions("order-placed")] == ["warehouse"], "python admin: update_subscription; update_topic keeps the subscriptions")
    with ServiceBusClient.from_connection_string(cs) as sb:
        with sb.get_queue_sender("payments-retry") as tx:
            tx.send_messages(ServiceBusMessage('{"paymentId":"pay_7781"}', subject="Retry"))
        with sb.get_queue_receiver("payments-retry", max_wait_time=10) as rx:
            got = rx.receive_messages(max_wait_time=10)
            for m in got:
                rx.complete_message(m)
        check([m.subject for m in got] == ["Retry"], "python: send and receive over AMQP on the management port")

    # .NET: the official SDK's admin client over plain HTTP on the management port.
    proj = os.path.join(os.path.dirname(os.path.abspath(__file__)), "servicebus_admin_dotnet")
    if shutil.which("dotnet"):
        s.call("POST", "/services/azure-service-bus/accounts", {"account": "sb-admin-net"})
        cs_net = cs.replace(f"sb://{ns}.", "sb://sb-admin-net.")
        out = subprocess.run(["dotnet", "run", "--project", proj, "--", cs_net], capture_output=True, text=True, timeout=600)
        print(out.stdout.strip())
        check(out.returncode == 0, ".NET ServiceBusAdministrationClient + ServiceBusClient on the management port")
    else:
        print("skip .NET: no dotnet")
    shutil.rmtree(tmp, ignore_errors=True)

print("servicebus management + test CA: all checks passed" if failed == 0 else f"servicebus management + test CA: {failed} failed")
sys.exit(1 if failed else 0)
