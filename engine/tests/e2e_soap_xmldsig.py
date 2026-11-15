"""End-to-end check of A18: a SEFAZ-like SOAP service with mTLS that requires an ICP-Brasil style
certificate (a CNPJ in it), checks the request's XML signature and signs its answers. The test
certificates come from new_test_certificate; an independent library (signxml) checks the
signatures both ways.

    pip install signxml
    cargo build -p stuntman && python3 tests/e2e_soap_xmldsig.py
"""
import base64, http.client, os, socket, ssl, tempfile

from harness import Stuntman

PLUGIN = """
service:
  name: sefaz-hom
  title: SEFAZ (homologação, simulada)
  real_host: hom.sefaz.example
  auth:
    client_certificate:
      required: true
      icp: true
soap:
  version: '1.2'
  endpoint: /ws/RecepcaoEvento
  validate: false
  signature:
    check: true
    invalid: signature_invalid
    sign: infEvento
operations:
  - operation: nfeRecepcaoEvento
    action: http://www.portalfiscal.inf.br/nfe/wsdl/NFeRecepcaoEvento4/nfeRecepcaoEvento
    decide:
      - when: client_cnpj == '11222333000181'
        answer: registered
      - answer: wrong_cnpj
answers:
  - name: registered
    body: '<retEnvEvento xmlns="http://www.portalfiscal.inf.br/nfe" versao="1.00"><cStat>128</cStat><retEvento versao="1.00"><infEvento Id="ID891261111110"><cStat>135</cStat><xMotivo>Evento registrado e vinculado a NF-e</xMotivo></infEvento></retEvento></retEnvEvento>'
  - name: wrong_cnpj
    body: '<retEnvEvento xmlns="http://www.portalfiscal.inf.br/nfe" versao="1.00"><cStat>489</cStat><xMotivo>Rejeicao: CNPJ do certificado difere</xMotivo></retEnvEvento>'
  - name: signature_invalid
    body: '<retEnvEvento xmlns="http://www.portalfiscal.inf.br/nfe" versao="1.00"><cStat>297</cStat><xMotivo>Rejeicao: Assinatura difere do calculado</xMotivo></retEnvEvento>'
"""

EVENT = '<envEvento xmlns="http://www.portalfiscal.inf.br/nfe" versao="1.00"><idLote>1</idLote><evento versao="1.00"><infEvento Id="ID1101113526111122233300018155001000001234100001234501"><cOrgao>35</cOrgao><tpAmb>2</tpAmb><CNPJ>11222333000181</CNPJ><tpEvento>110111</tpEvento></infEvento></evento></envEvento>'


def envelope(body):
    return f'<soap12:Envelope xmlns:soap12="http://www.w3.org/2003/05/soap-envelope"><soap12:Body><nfeDadosMsg xmlns="http://www.portalfiscal.inf.br/nfe/wsdl/NFeRecepcaoEvento4">{body}</nfeDadosMsg></soap12:Body></soap12:Envelope>'


def tls_post(s, cert, body):
    ctx = ssl.create_default_context(cafile=s.ca)
    if cert:
        ctx.load_cert_chain(cert[0], cert[1])
    c = http.client.HTTPSConnection("127.0.0.1", s.APIS_TLS_PORT, context=ctx, timeout=20)
    c.sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", s.APIS_TLS_PORT)), server_hostname="hom.sefaz.example")
    c.request("POST", "/ws/RecepcaoEvento", envelope(body).encode(), {"Content-Type": 'application/soap+xml; charset=utf-8; action="http://www.portalfiscal.inf.br/nfe/wsdl/NFeRecepcaoEvento4/nfeRecepcaoEvento"', "Host": "hom.sefaz.example"})
    r = c.getresponse()
    return r.status, r.read().decode()


def files(cert):
    d = tempfile.mkdtemp()
    c, k = os.path.join(d, "c.pem"), os.path.join(d, "k.pem")
    open(c, "w").write(cert["certificate"])
    open(k, "w").write(cert["key"])
    return c, k


def main():
    from signxml import XMLVerifier, SignatureConfiguration, SignatureMethod, DigestAlgorithm
    sha1_ok = SignatureConfiguration(signature_methods=frozenset(SignatureMethod), digest_algorithms=frozenset(DigestAlgorithm))
    with Stuntman("stuntman-a18-") as s:
        pem = s.call("GET", "/apis/test-ca")["pem"]
        fd, s.ca = tempfile.mkstemp(suffix=".pem")
        os.write(fd, pem.encode())
        os.close(fd)
        s.call("PUT", "/plugins/sefaz-hom", {"yaml": PLUGIN})
        acme = s.call("POST", "/apis/test-certificates", {"holder": "Acme Transportes Ltda", "cnpj": "11.222.333/0001-81"})
        assert acme["cnpj"] == "11222333000181" and acme["pfx"] and acme["password"], acme
        # The .pfx opens with its password and holds the key and the chain.
        from cryptography.hazmat.primitives.serialization import pkcs12
        k, c, extra = pkcs12.load_key_and_certificates(base64.b64decode(acme["pfx"]), acme["password"].encode())
        assert k is not None and c is not None and len(extra) >= 1
        globex = s.call("POST", "/apis/test-certificates", {"holder": "Globex Logistica", "cnpj": "44.555.666/0001-72"})
        person = s.call("POST", "/apis/test-certificates", {"holder": "Ana Lima", "cpf": "111.444.777-35"})
        old = s.call("POST", "/apis/test-certificates", {"holder": "Acme Transportes Ltda", "cnpj": "11.222.333/0001-81", "expired": True})
        lst = s.call("GET", "/apis/test-certificates")["certificates"]
        assert {x["state"] for x in lst} == {"valid", "expired"}, lst

        # The app signs the event (sign_xml), signxml agrees, the service checks it and signs back.
        signed = s.call("POST", "/apis/xml/sign", {"xml": EVENT, "id": "ID1101113526111122233300018155001000001234100001234501", "certificate": acme["id"]})["xml"]
        XMLVerifier().verify(signed.encode(), x509_cert=acme["certificate"], expect_config=sha1_ok)
        assert s.call("POST", "/apis/xml/verify", {"xml": signed})["signatures"][0]["cnpj"] == "11222333000181"
        st, body = tls_post(s, files(acme), signed)
        assert st == 200 and "<cStat>135</cStat>" in body, (st, body)
        inner = body[body.index("<retEnvEvento"):body.index("</retEnvEvento>") + len("</retEnvEvento>")]
        v = s.call("POST", "/apis/xml/verify", {"xml": inner})
        assert v["valid"] and v["signatures"][0]["reference"] == "ID891261111110" and v["signatures"][0]["cnpj"] == "11222333000181", v
        cert_b64 = inner.split("<X509Certificate>")[1].split("<")[0]
        XMLVerifier().verify(inner.encode(), x509_cert="-----BEGIN CERTIFICATE-----\n" + cert_b64 + "\n-----END CERTIFICATE-----", expect_config=sha1_ok)

        # Tampered and unsigned: cStat 297.
        st, body = tls_post(s, files(acme), signed.replace("<tpAmb>2</tpAmb>", "<tpAmb>1</tpAmb>"))
        assert "<cStat>297</cStat>" in body, body
        st, body = tls_post(s, files(acme), EVENT)
        assert "<cStat>297</cStat>" in body, body
        # Another company's certificate: the rule sees its CNPJ.
        st, body = tls_post(s, files(globex), s.call("POST", "/apis/xml/sign", {"xml": EVENT, "id": "ID1101113526111122233300018155001000001234100001234501", "certificate": globex["id"]})["xml"])
        assert "<cStat>489</cStat>" in body, body

        # TLS refusals: no certificate, an e-CPF (no CNPJ), an expired one.
        for cert, why in [(None, "none"), (files(person), "e-CPF"), (files(old), "expired")]:
            try:
                tls_post(s, cert, signed)
                raise AssertionError(f"{why}: TLS should fail")
            except (ssl.SSLError, ConnectionError, http.client.HTTPException, OSError):
                pass
        calls = s.call("GET", "/apis/calls?service=sefaz-hom")["calls"]
        assert any("no CNPJ" in (c.get("sentence") or "") for c in calls), [c.get("sentence") for c in calls]
    print("A18 e2e: ok")


if __name__ == "__main__":
    main()
