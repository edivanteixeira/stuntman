"""SERPRO Datavalid V5 and GCC consent as a .NET client calls them (an auth service, a Datavalid client,
a consent client), against the serpro-datavalid and gcc-consent catalog plugins. Every situation
an antifraud test suite uses is reached by its CPF or by force_answer,
and the answer is read the way the .NET client reads it.

Run with Stuntman up (it creates, then deletes, an environment of its own):
    STUNTMAN=http://127.0.0.1:8025 STUNTMAN_APIS=http://127.0.0.1:8028 python3 tests/e2e_serpro.py
"""
import base64, json, os, socket, sys, urllib.error, urllib.parse, urllib.request, uuid

STUNTMAN = os.environ.get("STUNTMAN", "http://127.0.0.1:8025")
APIS = os.environ.get("STUNTMAN_APIS", "http://127.0.0.1:8028")
ENV = os.environ.get("STUNTMAN_ENV_NAME", "serpro-e2e")
CTL = f"{STUNTMAN}/e/{ENV}/api"
SERPRO = f"{APIS}/e/{ENV}/serpro-datavalid"
GCC = f"{APIS}/e/{ENV}/gcc-consent"


def http(method, url, body=None, headers=None, form=False, timeout=30):
    h = dict(headers or {})
    data = None
    if body is not None:
        data = (body if isinstance(body, str) else urllib.parse.urlencode(body)).encode() if form else json.dumps(body).encode()
        h["Content-Type"] = "application/x-www-form-urlencoded; charset=utf-8" if form else "application/json; charset=utf-8"
    req = urllib.request.Request(url, data=data, method=method, headers={"X-Stuntman-Client": "e2e-serpro", **h})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def ctl(method, path, body=None, expect=200):
    st, t = http(method, CTL + path, body)
    assert st == expect, (method, path, st, t)
    return json.loads(t or "null")


# ---------------------------------------------------------------- the .NET client, in Python

def token(key="mock-consumer-key", secret="mock-consumer-secret"):
    """Auth service: POST TokenEndpoint, Basic key:secret, form grant_type=client_credentials."""
    basic = base64.b64encode(f"{key}:{secret}".encode()).decode()
    st, t = http("POST", f"{SERPRO}/token", "grant_type=client_credentials", {"Authorization": f"Basic {basic}"}, form=True)
    return st, (json.loads(t) if st == 200 else t)


PRIV = {"rfb": {"id_template": "mock-rfb-template"}, "senatran": {"token": "eyJ.mock.hash", "cnpj_anuente": "11222333000181"}}
FOTO = base64.b64encode(b"\xff\xd8\xff\xe0" + b"selfie" * 400).decode()


def facial_body(cpf, nome="Emma Carter", priv=PRIV):
    return {"privacidade": priv, "cpf": cpf, "validacao": {"nome": nome, "rfb": {"situacao_cpf": "REGULAR"},
            "biometria_facial": {"formato": "JPG", "base64": FOTO, "vivacidade": True}}}


def basica_body(cpf, nome="Emma Carter", nascimento=None):
    v = {"nome": nome, "rfb": {"situacao_cpf": "REGULAR"}}
    if nascimento:
        v["data_nascimento"] = nascimento
    return {"privacidade": PRIV, "cpf": cpf, "validacao": v}


def privacy_error(code):
    return isinstance(code, str) and len(code) == 5 and code.startswith("DV") and code[2] in "23"


def parse_error(body):
    try:
        root = json.loads(body)
        code = root.get("codigo", root.get("code", ""))
        msg = root.get("mensagem", root.get("message", body))
        return code or "", msg
    except ValueError:
        return "UNKNOWN", body


def validacao(body, bearer, product="datavalid", timeout=30):
    """Datavalid client, facial and basic checks: same path, same result rules."""
    st, t = http("POST", f"{SERPRO}/{product}/v5/pessoa-fisica/validacao", body,
                 {"Authorization": f"Bearer {bearer}", "x-request-trace-id": str(uuid.uuid4())}, timeout=timeout)
    if st == 204:
        return {"Success": False, "IsProviderFailure": False, "ErrorCode": "NO_CONTENT", "status": st}
    if st == 422:
        code, msg = parse_error(t)
        return {"Success": False, "IsProviderFailure": privacy_error(code), "ErrorCode": code, "ErrorMessage": msg, "status": st}
    if not 200 <= st < 300:
        return {"Success": False, "IsProviderFailure": True, "ErrorCode": f"HTTP_{st}", "status": st, "body": t}
    try:
        r = json.loads(t)
        # System.Text.Json: rfb_existe/cnh_existe bool, biometria_facial.disponivel bool and similaridade decimal (not null).
        assert isinstance(r, dict) and isinstance(r.get("rfb_existe", False), bool)
        bf = r.get("biometria_facial")
        if bf is not None:
            assert isinstance(bf.get("disponivel", False), bool) and isinstance(bf.get("similaridade", 0), (int, float)) and not isinstance(bf.get("similaridade", 0), bool)
    except (ValueError, AssertionError):
        return {"Success": False, "IsProviderFailure": True, "ErrorCode": "PARSE_ERROR", "status": st}
    rfb, bf = r.get("rfb") or {}, r.get("biometria_facial") or {}
    return {"Success": True, "status": st, "raw": r, "RfbExiste": r.get("rfb_existe", False), "SituacaoCpfRegular": rfb.get("situacao_cpf"),
            "NomeSimilaridade": rfb.get("nome_similaridade"), "DataNascimentoMatch": rfb.get("data_nascimento"),
            "BiometriaDisponivel": bf.get("disponivel", False), "Similaridade": bf.get("similaridade", 0), "Vivacidade": bf.get("vivacidade"),
            "Probabilidade": bf.get("probabilidade", ""), "ErrorCode": bf.get("codigo_retorno"), "HasBiometria": "biometria_facial" in r}


def gcc(body, api_key="mock-gcc-api-key"):
    """GccConsentClient.RegisterConsentAsync: POST /consentimentos-ciencias with Api-Key."""
    st, t = http("POST", f"{GCC}/consentimentos-ciencias", body, {"Api-Key": api_key} if api_key else {})
    if not 200 <= st < 300:
        return {"Success": False, "IsProviderFailure": True, "ErrorCode": f"HTTP_{st}"}
    try:
        r = json.loads(t)
    except ValueError:
        return {"Success": False, "IsProviderFailure": True, "ErrorCode": "PARSE_ERROR"}
    return {"Success": True, "NumeroHashConsulta": r.get("numero_hash_consulta"), "Status": r.get("status"), "Critica": r.get("critica")}


def gcc_body(cpf, template="mock-template"):
    return {"tipo_titular": 1, "cpf_cnpj": cpf, "tipo_de_usuario": 1, "cnpj_anuente": "11222333000181", "cnpj_provedor": "11222333000181", "template_id": template}


def jwt_payload(t):
    p = t.split(".")[1]
    return json.loads(base64.urlsafe_b64decode(p + "=" * (-len(p) % 4)))


def force(service, answer, **kw):
    ctl("POST", f"/apis/{service}/force", {"answer": answer, **kw})


# ---------------------------------------------------------------- the situations

def run():
    st, tok = token()
    assert st == 200 and tok["token_type"] == "Bearer" and tok["access_token"] and tok["expires_in"] == 3600, (st, tok)
    B = tok["access_token"]

    # Face + liveness (PostFacialAsync), by CPF: the old serpro-mock.py scenarios.
    facial = {
        "123.456.789-09": dict(Success=True, Similaridade=0.99, Vivacidade="real", SituacaoCpfRegular=True, NomeSimilaridade=0.98, BiometriaDisponivel=True),  # any CPF: match
        "11111111111": dict(Success=True, Similaridade=0.99, Vivacidade="real", SituacaoCpfRegular=True, NomeSimilaridade=0.98),                              # match
        "11122211155": dict(Success=True, Similaridade=0.99, NomeSimilaridade=0.10),                                                                         # match_sem_nome
        "22222222222": dict(Success=True, Similaridade=0.30, Vivacidade="real"),                                                                            # nomatch
        "33333333333": dict(Success=True, Similaridade=0.80, NomeSimilaridade=0.80),                                                                        # borderline
        "44444444444": dict(Success=True, Similaridade=0.99, Vivacidade="fake"),                                                                            # vivacidade_fake
        "44455544488": dict(Success=True, Vivacidade="bad_quality"),                                                                                        # vivacidade_bad_quality
        "55555555555": dict(Success=True, Similaridade=0.99, NomeSimilaridade=None),                                                                        # nome_degradado
        "66666666666": dict(Success=True, Similaridade=0.99, SituacaoCpfRegular=False),                                                                     # cpf_irregular
        "10101010133": dict(Success=True, BiometriaDisponivel=False, Similaridade=0, Vivacidade=None, Probabilidade=""),                                    # sem_biometria
        "77777777777": dict(Success=False, IsProviderFailure=False, ErrorCode="NO_CONTENT", status=204),                                                   # sem_conteudo_204
        "88888888888": dict(Success=False, IsProviderFailure=False, ErrorCode="DV040", ErrorMessage="CPF invalido", status=422),                          # erro_422_dv040
        "88899988811": dict(Success=False, IsProviderFailure=False, ErrorCode="DV045", status=422),                                                         # erro_422_dv045
        "99999999999": dict(Success=False, IsProviderFailure=True, ErrorCode="HTTP_500"),                                                                  # erro_500
        "99988899955": dict(Success=False, IsProviderFailure=True, ErrorCode="HTTP_401"),                                                                  # erro_401
        "13131313188": dict(Success=False, IsProviderFailure=True, ErrorCode="PARSE_ERROR", status=200),                                                   # json_quebrado
    }
    for cpf, want in facial.items():
        got = validacao(facial_body(cpf), B)
        for k, v in want.items():
            assert got.get(k) == v, (cpf, k, got.get(k), v, got)
    # Formatted CPF finds the same row; the record of the call says which answer it was (what __requests said).
    assert validacao(facial_body("222.222.222-22"), B)["Similaridade"] == 0.30
    calls = ctl("GET", "/apis/calls?service=serpro-datavalid&q=22222222222")["calls"]
    assert calls[0]["answer"] == "face_mismatch" and calls[0]["status"] == 200, calls[0]
    full = ctl("GET", f"/apis/calls/{calls[0]['id']}?raw=true")
    sent = json.loads(full["request"]["body"])
    assert sent["validacao"]["nome"] == "Emma Carter" and len(sent["validacao"]["biometria_facial"]["base64"]) > 1000, sent["validacao"].keys()

    # timeout: Stuntman holds the connection; a client's 30 s attempt timeout gives up (2 s here).
    try:
        validacao(facial_body("12121212108"), B, timeout=2)
        raise AssertionError("timeout scenario answered")
    except (socket.timeout, TimeoutError, urllib.error.URLError):
        pass

    # CPF only (PostBasicaAsync): same path, no biometria_facial in the answer.
    r = validacao(basica_body("11111111111", nascimento="1990-01-01"), B)
    assert r["Success"] and r["RfbExiste"] and r["SituacaoCpfRegular"] is True and r["NomeSimilaridade"] == 0.98 and r["DataNascimentoMatch"] is True and not r["HasBiometria"], r
    r = validacao(basica_body("11111111111"), B)
    assert r["Success"] and r["DataNascimentoMatch"] is None, r
    r = validacao(basica_body("66666666666"), B)
    assert r["Success"] and r["SituacaoCpfRegular"] is False and not r["HasBiometria"], r
    r = validacao(basica_body("22222222222"), B)
    assert r["Success"] and r["SituacaoCpfRegular"] is True and not r["HasBiometria"], "a face mismatch CPF passes the CPF-only check"
    r = validacao(basica_body("11122211155"), B)
    assert r["NomeSimilaridade"] == 0.10, r
    assert validacao(basica_body("77777777777"), B)["ErrorCode"] == "NO_CONTENT"
    r = validacao(basica_body("88888888888"), B)
    assert r["ErrorCode"] == "DV040" and r["IsProviderFailure"] is False, r
    assert validacao(basica_body("99999999999"), B)["ErrorCode"] == "HTTP_500"

    # Demo gateway (UseDemoMode=true) answers the same; another product is 404.
    r = validacao(facial_body("22222222222"), B, product="datavalid-demonstracao")
    assert r["Success"] and r["Similaridade"] == 0.30, r
    assert validacao(facial_body("11111111111"), B, product="datavalid-v9")["ErrorCode"] == "HTTP_404"

    # Configuration errors are provider failures: no RFB template (DV207), GCC hash refused (DV300).
    r = validacao(facial_body("11111111111", priv={"rfb": {}, "senatran": {"token": ""}}), B)
    assert r["ErrorCode"] == "DV207" and r["IsProviderFailure"] is True, r
    force("serpro-datavalid", "gcc_hash_refused")
    r = validacao(facial_body("11111111111"), B)
    assert r["ErrorCode"] == "DV300" and r["IsProviderFailure"] is True, r

    # Forced answers (POST /__scenario in the old mock): the next call, either kind.
    for answer, check in [("face_mismatch", lambda r: r["Similaridade"] == 0.30), ("liveness_fake", lambda r: r["Vivacidade"] == "fake"),
                          ("no_biometrics", lambda r: r["BiometriaDisponivel"] is False), ("not_found", lambda r: r["ErrorCode"] == "NO_CONTENT"),
                          ("bad_photo", lambda r: r["ErrorCode"] == "DV045"), ("token_expired", lambda r: r["ErrorCode"] == "HTTP_401"),
                          ("malformed_json", lambda r: r["ErrorCode"] == "PARSE_ERROR"), ("500", lambda r: r["ErrorCode"] == "HTTP_500"),
                          ("serpro_down", lambda r: r["ErrorCode"] == "HTTP_503"), ("cpf_irregular", lambda r: r["SituacaoCpfRegular"] is False)]:
        force("serpro-datavalid", answer)
        r = validacao(facial_body("123.456.789-09"), B)
        assert check(r), (answer, r)
    force("serpro-datavalid", "face_mismatch")
    r = validacao(basica_body("123.456.789-09"), B)
    assert r["Success"] and r["SituacaoCpfRegular"] is True and not r["HasBiometria"], "a forced face answer to the CPF-only call has no face block"
    force("serpro-datavalid", "face_mismatch", count=1)
    st, _ = token()
    assert st == 200, "a service-wide force never hits the token route"
    assert validacao(facial_body("123.456.789-09"), B)["Similaridade"] == 0.30
    assert validacao(facial_body("123.456.789-09"), B)["Similaridade"] == 0.99, "count=1 ends"

    # Token: refused credentials (token_invalido), only by naming the route; no or expired bearer.
    force("serpro-datavalid", "token_refused", route="POST /token")
    st, t = token()
    assert st == 401 and json.loads(t)["error"] == "invalid_client", (st, t)
    assert token()[0] == 200
    assert validacao(facial_body("11111111111"), "")["ErrorCode"] == "HTTP_401"
    ctl("POST", "/plugins/serpro-datavalid/tokens/expire")
    r = validacao(facial_body("11111111111"), B)
    assert r["ErrorCode"] == "HTTP_401" and "expired" in r["body"], r

    # GCC: registered with a JWT-like hash (jti), a critica without hash, Api-Key, failures.
    r = gcc(gcc_body("123.456.789-09"))
    assert r["Success"] and r["Status"] == "REGISTRADO" and r["Critica"] is None, r
    h = r["NumeroHashConsulta"]
    assert h.count(".") == 2 and jwt_payload(h)["jti"] and jwt_payload(h)["sub"] == "12345678909" and isinstance(jwt_payload(h)["exp"], int) and jwt_payload(h)["exp"] > jwt_payload(h)["iat"], h
    r = gcc(gcc_body("55566655599"))
    assert r["Success"] and r["NumeroHashConsulta"] is None and r["Critica"] == "template não encontrado", r
    r = gcc(gcc_body("123.456.789-09", template=""))
    assert r["NumeroHashConsulta"] is None and r["Critica"], r
    assert gcc(gcc_body("123.456.789-09"), api_key=None)["ErrorCode"] == "HTTP_401"
    force("gcc-consent", "api_key_expired")
    assert gcc(gcc_body("123.456.789-09"))["ErrorCode"] == "HTTP_401"
    force("gcc-consent", "500")
    assert gcc(gcc_body("123.456.789-09"))["ErrorCode"] == "HTTP_500"
    force("gcc-consent", "critica")
    assert gcc(gcc_body("123.456.789-09"))["Critica"] == "template não encontrado"


def main():
    st, t = http("POST", f"{STUNTMAN}/api/environments", {"name": ENV})
    assert st in (200, 201), (st, t)
    try:
        for p in ["serpro-datavalid", "gcc-consent"]:
            ctl("POST", "/plugins", {"plugin": p})
            assert ctl("POST", f"/plugins/{p}/validate", {})["ok"], p
            ctl("PUT", f"/plugins/{p}/service", {"latency": "none", "rate": "none"})
        run()
    finally:
        http("DELETE", f"{STUNTMAN}/api/environments/{ENV}")
    print("e2e_serpro: ok")


if __name__ == "__main__":
    sys.exit(main())
