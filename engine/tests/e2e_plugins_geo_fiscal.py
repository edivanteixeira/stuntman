"""End-to-end check of the catalog plugins osrm, google-maps, nominatim, ocr-space, receitaws and
sefaz-nfe: each is installed from engine/plugins and its main routes and refusals are called. The
SEFAZ one goes over HTTPS with an e-CNPJ test certificate and an event signed by sign_xml.

    cargo build -p stuntman && python3 tests/e2e_plugins_geo_fiscal.py
"""
import http.client, json, os, socket, ssl, tempfile
from urllib.parse import quote

from harness import Stuntman

PLUGINS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "plugins")


def install(s, name):
    r = s.call("PUT", f"/plugins/{name}", {"yaml": open(os.path.join(PLUGINS, name + ".yaml")).read()})
    assert not r.get("errors"), r


def get(s, path, headers=None):
    st, v, _ = s.api("GET", path, headers=headers)
    return st, v


def osrm(s):
    install(s, "osrm")
    st, v = get(s, "/osrm/route/v1/driving/-46.6527,-23.5643;-46.6869,-23.5781?geometries=geojson")
    assert st == 200 and v["code"] == "Ok", v
    r = v["routes"][0]
    assert 4000 < r["distance"] < 7000 and r["duration"] > 0 and r["legs"][0]["distance"] == r["distance"], r
    assert r["geometry"]["coordinates"] == [[-46.6527, -23.5643], [-46.6869, -23.5781]] and len(v["waypoints"]) == 2, r
    st, v = get(s, "/osrm/route/v1/foot/-46.6527,-23.5643;-46.6869,-23.5781")
    assert v["routes"][0]["duration"] > r["duration"] * 5 and isinstance(v["routes"][0]["geometry"], str), v
    st, v = get(s, "/osrm/table/v1/driving/-46.6527,-23.5643;-46.6869,-23.5781?annotations=distance,duration")
    assert st == 200 and v["durations"][0][0] == 0 and v["durations"][0][1] > 0 and v["distances"][0][1] > 4000, v
    st, v = get(s, "/osrm/nearest/v1/driving/-46.6527,-23.5643")
    assert st == 200 and v["waypoints"][0]["location"] == [-46.6527, -23.5643], v
    st, v = get(s, "/osrm/trip/v1/driving/-46.6527,-23.5643;-46.6869,-23.5781")
    assert st == 200 and len(v["trips"][0]["legs"]) == 2 and v["waypoints"][1]["waypoint_index"] == 1, v
    st, v = get(s, "/osrm/route/v1/driving/-46.65,-23.56;-74.00,40.71")
    assert st == 400 and v["code"] == "NoRoute", v
    st, v = get(s, "/osrm/route/v1/driving/garbage")
    assert st == 400 and v["code"] == "InvalidQuery", v
    st, v = get(s, "/osrm/route/v1/driving/-46.65,-23.56;-46.66,-23.57;-46.68,-23.58")
    assert st == 200 and len(v["routes"][0]["legs"]) == 2, v


def google(s):
    install(s, "google-maps")
    g = "/google-maps/maps/api"
    st, v = get(s, g + "/geocode/json?address=" + quote("Rua das Palmeiras, 120, São Paulo") + "&key=AIzaTest")
    assert st == 200 and v["status"] == "OK", v
    res = v["results"][0]
    assert res["geometry"]["location"] == {"lat": -23.5643, "lng": -46.6527} and res["place_id"] == "ChIJSynthPalmeiras120SaoPaulo", res
    assert any(c["short_name"] == "SP" for c in res["address_components"]), res
    st, v = get(s, g + "/geocode/json?latlng=39.7817,-89.6501&key=AIzaTest")
    assert v["results"][0]["formatted_address"].startswith("123 Example St"), v
    st, v = get(s, g + "/geocode/json?latlng=10.5,-20.25&key=AIzaTest")
    assert v["results"][0]["geometry"]["location"] == {"lat": 10.5, "lng": -20.25}, v
    assert get(s, g + "/geocode/json?address=Nowhere%20Lane&key=AIzaTest")[1] == {"results": [], "status": "ZERO_RESULTS"}
    v = get(s, g + "/geocode/json?address=x")[1]
    assert v["status"] == "REQUEST_DENIED" and "API key" in v["error_message"], v
    assert get(s, g + "/geocode/json?address=x&key=invalid-1")[1]["status"] == "REQUEST_DENIED"
    assert get(s, g + "/geocode/json?address=x&key=over-quota")[1]["status"] == "OVER_QUERY_LIMIT"
    v = get(s, g + "/geocode/json?key=AIzaTest")[1]
    assert v["status"] == "INVALID_REQUEST" and "'latlng'" in v["error_message"], v
    st, v = get(s, g + "/directions/json?origin=" + quote("rua alfa, 42, são paulo") + "&destination=place_id:ChIJSynthBeta300SaoPaulo&key=AIzaTest")
    leg = v["routes"][0]["legs"][0]
    assert v["status"] == "OK" and 5000 < leg["distance"]["value"] < 12000 and leg["distance"]["text"].endswith(" km"), v
    assert leg["end_address"].startswith("Alameda Beta, 300") and leg["steps"][0]["travel_mode"] == "DRIVING", leg
    v = get(s, g + "/directions/json?origin=" + quote("rua alfa, 42, são paulo") + "&destination=" + quote("500 sample avenue, austin, tx") + "&key=AIzaTest")[1]
    assert v["status"] == "ZERO_RESULTS", v
    assert get(s, g + "/directions/json?origin=Nowhere&destination=x&key=AIzaTest")[1]["status"] == "NOT_FOUND"
    v = get(s, g + "/distancematrix/json?origins=" + quote("rua alfa, 42, são paulo") + "&destinations=" + quote("avenida dos testes, 1500, são paulo") + "&key=AIzaTest")[1]
    el = v["rows"][0]["elements"][0]
    assert v["status"] == "OK" and el["status"] == "OK" and el["distance"]["value"] > 1000 and v["origin_addresses"][0].startswith("R. Alfa"), v
    assert get(s, g + "/distancematrix/json?origins=Nowhere&destinations=x&key=k")[1]["rows"][0]["elements"][0]["status"] == "NOT_FOUND"
    v = get(s, g + "/place/autocomplete/json?input=Rua&key=AIzaTest")[1]
    assert v["status"] == "OK" and len(v["predictions"]) == 3 and v["predictions"][0]["matched_substrings"][0]["length"] == 3, v
    v = get(s, g + "/place/details/json?place_id=ChIJSynthAlfa42SaoPaulo&key=AIzaTest")[1]
    assert v["status"] == "OK" and v["result"]["name"] == "R. Alfa, 42" and v["result"]["utc_offset"] == -180, v
    assert get(s, g + "/place/details/json?place_id=nope&key=AIzaTest")[1]["status"] == "NOT_FOUND"
    b = [x for x in s.call("GET", "/apis/billing?service=google-maps")["services"] if x["service"] == "google-maps"][0]
    assert b["billable"] == b["calls"] == 16 and abs(b["total"] - b["billable"] * 0.005) < 1e-9, b


def nominatim(s):
    install(s, "nominatim")
    st, v = get(s, "/nominatim/search?q=" + quote("Rua Alfa, São Paulo") + "&format=json&addressdetails=1")
    assert st == 200 and len(v) == 1 and v[0]["class"] == "highway" and v[0]["lat"] == "-23.5534000" and v[0]["address"]["postcode"] == "01317-000", v
    v = get(s, "/nominatim/search?q=" + quote("rua alfa, são paulo"))[1]
    assert "category" in v[0] and "class" not in v[0] and "address" not in v[0], v
    assert get(s, "/nominatim/search?q=Atlantis&format=json")[1] == []
    st, v = get(s, "/nominatim/reverse?lat=-23.5640&lon=-46.6522&format=jsonv2")
    assert st == 200 and v["name"] == "Rua das Palmeiras" and v["address"]["country_code"] == "br", v
    assert get(s, "/nominatim/reverse?lat=0&lon=0&format=json")[1] == {"error": "Unable to geocode"}
    assert get(s, "/nominatim/reverse?lat=1&format=json")[0] == 400
    st, v = get(s, "/nominatim/search?q=x&format=json", headers={"User-Agent": ""})
    assert st == 403 and b"Access blocked" in v, (st, v)


def multipart(fields, file=None):
    b = "----stuntmanboundary7"
    out = b""
    for k, val in fields.items():
        out += f'--{b}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{val}\r\n'.encode()
    if file:
        out += f'--{b}\r\nContent-Disposition: form-data; name="file"; filename="{file[0]}"\r\nContent-Type: image/png\r\n\r\n'.encode() + file[1] + b"\r\n"
    return out + f"--{b}--\r\n".encode(), {"Content-Type": f"multipart/form-data; boundary={b}"}


def ocr(s):
    install(s, "ocr-space")
    body, h = multipart({"language": "eng", "OCREngine": "2"}, ("receipt.jpg", b"\x89PNG fake image bytes"))
    st, v, _ = s.api("POST", "/ocr-space/parse/image", body, dict(h, apikey="helloworld"))
    assert st == 200 and v["OCRExitCode"] == 1 and not v["IsErroredOnProcessing"], v
    pr = v["ParsedResults"][0]
    assert pr["FileParseExitCode"] == 1 and pr["ParsedText"].startswith("GLOBEX MARKET\r\n") and pr["ErrorMessage"] == "", pr
    assert v["ProcessingTimeInMilliseconds"].isdigit(), v
    form = {"Content-Type": "application/x-www-form-urlencoded"}
    st, v, _ = s.api("POST", "/ocr-space/parse/image", b"apikey=helloworld&base64Image=data%3Aimage%2Fpng%3Bbase64%2CiVBORw0KGgo%3D", form)
    assert st == 200 and v["ParsedResults"][0]["ParsedText"].startswith("INVOICE 2026-0042"), v
    st, v = get(s, "/ocr-space/parse/imageurl?apikey=helloworld&url=" + quote("https://files.example.com/cnh.jpg"))
    assert st == 200 and "MARIA EXEMPLO SILVA" in v["ParsedResults"][0]["ParsedText"], v
    st, v, _ = s.api("POST", "/ocr-space/parse/image", b"apikey=invalid-key&url=https%3A%2F%2Ffiles.example.com%2Fa.png", form)
    assert st == 403 and v["IsErroredOnProcessing"] and "invalid" in v["ErrorMessage"][0], v
    st, v, _ = s.api("POST", "/ocr-space/parse/image", b"url=https%3A%2F%2Ffiles.example.com%2Fa.png", form)
    assert st == 403, v
    body, h = multipart({"apikey": "helloworld"}, ("scan.png", b"x" * (1024 * 1024 + 10)))
    st, v, _ = s.api("POST", "/ocr-space/parse/image", body, h)
    assert st == 200 and v["OCRExitCode"] == 99 and "1024 KB" in v["ErrorMessage"][0], v


def receitaws(s):
    install(s, "receitaws")
    st, v = get(s, "/receitaws/v1/cnpj/11222333000181")
    assert st == 200 and v["status"] == "OK" and v["nome"] == "ACME TECNOLOGIA LTDA" and v["cnpj"] == "11.222.333/0001-81", v
    assert v["atividade_principal"][0]["code"] == "62.01-5-01" and v["situacao"] == "ATIVA" and v["uf"] == "SP" and v["municipio"] == "SAO PAULO", v
    assert get(s, "/receitaws/v1/cnpj/11222333000182")[1] == {"status": "ERROR", "message": "CNPJ inválido"}
    assert get(s, "/receitaws/v1/cnpj/24681357000140")[1]["message"] == "CNPJ rejeitado pela Receita Federal"
    st, v = get(s, "/receitaws/v1/cnpj/12345678000195")
    assert st == 429 and v == b"Too many requests, please try again later.", (st, v)


EVENT = ('<envEvento xmlns="http://www.portalfiscal.inf.br/nfe" versao="1.00"><idLote>202611150001</idLote><evento versao="1.00">'
         '<infEvento Id="ID1101113526111122233300018155001000001234100001234501"><cOrgao>35</cOrgao><tpAmb>2</tpAmb><CNPJ>11222333000181</CNPJ>'
         '<chNFe>35261111222333000181550010000012341000012345</chNFe><dhEvento>2026-11-15T10:00:00-03:00</dhEvento><tpEvento>110111</tpEvento>'
         '<nSeqEvento>1</nSeqEvento><verEvento>1.00</verEvento><detEvento versao="1.00"><descEvento>Cancelamento</descEvento>'
         '<nProt>135260000012345</nProt><xJust>Pedido cancelado pelo cliente antes do envio</xJust></detEvento></infEvento></evento></envEvento>')
EVENT_ID = "ID1101113526111122233300018155001000001234100001234501"
HOST = "homologacao.nfe.fazenda.sp.gov.br"


def tls_post(s, cert, payload):
    env = f'<soap12:Envelope xmlns:soap12="http://www.w3.org/2003/05/soap-envelope"><soap12:Body><nfeDadosMsg xmlns="http://www.portalfiscal.inf.br/nfe/wsdl/NFeRecepcaoEvento4">{payload}</nfeDadosMsg></soap12:Body></soap12:Envelope>'
    ctx = ssl.create_default_context(cafile=s.ca)
    if cert:
        ctx.load_cert_chain(cert[0], cert[1])
    c = http.client.HTTPSConnection("127.0.0.1", s.APIS_TLS_PORT, context=ctx, timeout=20)
    c.sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", s.APIS_TLS_PORT)), server_hostname=HOST)
    c.request("POST", "/ws/nferecepcaoevento4.asmx", env.encode(), {"Content-Type": 'application/soap+xml; charset=utf-8; action="http://www.portalfiscal.inf.br/nfe/wsdl/NFeRecepcaoEvento4/nfeRecepcaoEvento"', "Host": HOST})
    r = c.getresponse()
    return r.status, r.read().decode()


def pem_files(cert):
    d = tempfile.mkdtemp()
    c, k = os.path.join(d, "c.pem"), os.path.join(d, "k.pem")
    open(c, "w").write(cert["certificate"])
    open(k, "w").write(cert["key"])
    return c, k


def sefaz(s):
    install(s, "sefaz-nfe")
    fd, s.ca = tempfile.mkstemp(suffix=".pem")
    os.write(fd, s.call("GET", "/apis/test-ca")["pem"].encode())
    os.close(fd)
    acme = s.call("POST", "/apis/test-certificates", {"holder": "Acme Tecnologia Ltda", "cnpj": "11.222.333/0001-81"})
    globex = s.call("POST", "/apis/test-certificates", {"holder": "Globex Comercio Ltda", "cnpj": "44.555.666/0001-81"})
    sign = lambda xml, cert: s.call("POST", "/apis/xml/sign", {"xml": xml, "id": EVENT_ID, "certificate": cert["id"]})["xml"]
    signed = sign(EVENT, acme)

    st, body = tls_post(s, pem_files(acme), signed)
    assert st == 200 and "<cStat>128</cStat>" in body and "<cStat>135</cStat>" in body and "<xEvento>Cancelamento registrado</xEvento>" in body, (st, body)
    assert "<chNFe>35261111222333000181550010000012341000012345</chNFe>" in body and "-03:00</dhRegEvento>" in body, body
    inner = body[body.index("<retEnvEvento"):body.index("</retEnvEvento>") + len("</retEnvEvento>")]
    v = s.call("POST", "/apis/xml/verify", {"xml": inner})
    nprot = inner.split("<nProt>")[1].split("<")[0]
    assert v["valid"] and v["signatures"][0]["reference"] == "ID" + nprot, v

    for bad in [signed.replace("<xJust>Pedido", "<xJust>Pedidx"), EVENT]:
        st, body = tls_post(s, pem_files(acme), bad)
        assert "<cStat>297</cStat>" in body and "Assinatura difere do calculado" in body, body
    st, body = tls_post(s, pem_files(globex), sign(EVENT, globex))
    assert "<cStat>489</cStat>" in body, body
    st, body = tls_post(s, pem_files(acme), sign(EVENT.replace("<tpAmb>2</tpAmb>", "<tpAmb>1</tpAmb>"), acme))
    assert "<cStat>252</cStat>" in body, body
    try:
        tls_post(s, None, signed)
        raise AssertionError("no client certificate: TLS should fail")
    except (ssl.SSLError, ConnectionError, http.client.HTTPException, OSError):
        pass


def main():
    with Stuntman("stuntman-geo-fiscal-") as s:
        for f in (osrm, google, nominatim, ocr, receitaws, sefaz):
            f(s)
            print(f"{f.__name__}: ok")
    print("geo/fiscal plugins e2e: ok")


if __name__ == "__main__":
    main()
