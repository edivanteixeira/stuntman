"""What an app gets from Stuntman to start, and what warns before production: an app's KEY=VALUE
lines with a secret filled in (GET /api/env/<app>), the CA bundle, Service Bus entities made on
first use, two connections on one queue, and the short namespace list.

    cargo build -p stuntman && python3 tests/e2e_app_env.py
"""
import json, urllib.request

from azure.servicebus import ServiceBusClient, ServiceBusMessage

from harness import Stuntman


def text(s, path):
    with urllib.request.urlopen(f"http://127.0.0.1:{s.PORT}/api{path}", timeout=30) as r:
        return r.status, r.read().decode()


def main():
    with Stuntman("stuntman-appenv-") as s:
        env = s.call("GET", "/env")["env"]
        s.call("PUT", "/keyvault/vaults/acme-kv", {"any_token": True})
        s.call("PUT", "/keyvault/secrets/signing-password", {"value": "synthetic-signing-1", "vault": "acme-kv"})
        s.call("POST", "/services/azure-service-bus/accounts", {"account": "acme-bus"})
        s.call("PUT", f"/environments/{env}", {"apps": {
            "worker": {"BUS": "${QUEUES_CONNECTION}", "SIGNING_PASSWORD": "${secret:acme-kv/signing-password}", "IDENTITY_HEADER": "worker-mi", "NOPE": "x-${NOT_A_SETTING}"},
        }})

        # ---- an app's variables
        v = s.call("GET", f"/environments/{env}/apps/worker")
        assert v["vars"]["SIGNING_PASSWORD"] == "synthetic-signing-1" and v["vars"]["IDENTITY_HEADER"] == "worker-mi", v
        assert v["vars"]["BUS"].startswith("Endpoint=sb://acme-bus.") and v["missing"] == ["NOT_A_SETTING"] and v["vars"]["NOPE"] == "x-${NOT_A_SETTING}", v
        st, body = text(s, "/env/worker")
        assert st == 200 and "SIGNING_PASSWORD=synthetic-signing-1\n" in body and "IDENTITY_HEADER=worker-mi\n" in body, body
        log = s.call("GET", "/keyvault/log?vault=acme-kv")
        assert "app worker" in json.dumps(log), log
        s.call("GET", f"/environments/{env}/apps/nobody", expect=404)

        # ---- the CA bundle: public roots and the Test CA
        st, pem = text(s, "/ca-bundle.pem")
        _, ca = text(s, "/test-ca.pem")
        assert st == 200 and ca.strip() in pem and pem.count("BEGIN CERTIFICATE") > 1, pem[-400:]
        conns = s.call("GET", f"/environments/{env}/connections")
        assert any(i["key"] == "STUNTMAN_CA_BUNDLE" for i in conns["items"]), conns

        # ---- Service Bus: an entity the app made on first use, and two receivers on one queue
        cs = s.call("GET", "/messaging/namespaces?summary=true")["namespaces"][0]["connection"]
        with ServiceBusClient.from_connection_string(cs) as c, ServiceBusClient.from_connection_string(cs) as c2:
            with c.get_queue_sender("forgot-to-create") as snd:
                snd.send_messages(ServiceBusMessage("hi"))
            with c.get_queue_receiver("forgot-to-create", max_wait_time=2) as r1, c2.get_queue_receiver("forgot-to-create", max_wait_time=2) as r2:
                r1.receive_messages(max_message_count=1, max_wait_time=2)
                r2.receive_messages(max_message_count=1, max_wait_time=2)
                w = s.call("GET", "/messaging/warnings")["warnings"]
                kinds = {(x["kind"], x["entity"]) for x in w}
                assert ("created_on_first_use", "forgot-to-create") in kinds and ("several_receivers", "forgot-to-create") in kinds, w
        s.call("POST", "/services/azure-service-bus/accounts/acme-bus/entities", {"entity": "forgot-to-create"})
        assert not [x for x in s.call("GET", "/messaging/warnings")["warnings"] if x["kind"] == "created_on_first_use"], "kept in the YAML: no warning"

        # ---- the short namespace list
        full = json.dumps(s.call("GET", "/messaging/namespaces"))
        short = s.call("GET", "/messaging/namespaces?summary=true")
        n = short["namespaces"][0]
        assert set(n) == {"namespace", "kind", "connection", "port", "managementPort", "queues", "topics", "counts", "warnings"} and len(json.dumps(short)) * 3 < len(full), n
    print("app env, CA bundle and messaging warnings e2e: ok")


if __name__ == "__main__":
    main()
