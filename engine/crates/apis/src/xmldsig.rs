//! XML signatures (A14 SAML, A18 SOAP and fiscal documents): Canonical XML 1.0 (inclusive) and
//! Exclusive XML Canonicalization, enveloped signatures with RSA-SHA1 or RSA-SHA256, signed and
//! checked the way SEFAZ, SAML service providers and WS-Security stacks do.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use roxmltree::{Document, Node, NodeType};
use sha2::Digest;

pub const DSIG: &str = "http://www.w3.org/2000/09/xmldsig#";
pub const C14N: &str = "http://www.w3.org/TR/2001/REC-xml-c14n-20010315";
pub const C14N_COMMENTS: &str = "http://www.w3.org/TR/2001/REC-xml-c14n-20010315#WithComments";
pub const EXC_C14N: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";
pub const ENVELOPED: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";
pub const RSA_SHA1: &str = "http://www.w3.org/2000/09/xmldsig#rsa-sha1";
pub const RSA_SHA256: &str = "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256";
pub const SHA1: &str = "http://www.w3.org/2000/09/xmldsig#sha1";
pub const SHA256: &str = "http://www.w3.org/2001/04/xmlenc#sha256";

fn esc_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\r', "&#xD;")
}

fn esc_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;").replace('\t', "&#x9;").replace('\n', "&#xA;").replace('\r', "&#xD;")
}

fn qname(n: Node, uri: Option<&str>, local: &str) -> String {
    match uri {
        Some(u) => match n.lookup_prefix(u) {
            Some(p) if !p.is_empty() => format!("{p}:{local}"),
            _ => local.to_string(),
        },
        None => local.to_string(),
    }
}

/// The element's own prefix ("" for the default namespace or none).
fn prefix_of(n: Node) -> String {
    match n.tag_name().namespace() {
        Some(u) => n.lookup_prefix(u).unwrap_or("").to_string(),
        None => String::new(),
    }
}

struct C<'a> {
    exclusive: bool,
    comments: bool,
    /// Skip this node (the enveloped signature).
    skip: Option<roxmltree::NodeId>,
    out: String,
    _p: std::marker::PhantomData<&'a ()>,
}

impl<'a> C<'a> {
    fn element(&mut self, n: Node, rendered: &BTreeMap<String, String>, apex: bool) {
        if Some(n.id()) == self.skip {
            return;
        }
        // Namespace declarations to output here.
        let mut decls: BTreeMap<String, String> = BTreeMap::new();
        let in_scope: BTreeMap<String, String> = n.namespaces().map(|ns| (ns.name().unwrap_or("").to_string(), ns.uri().to_string())).filter(|(p, _)| p != "xml").collect();
        if self.exclusive {
            let mut used = vec![prefix_of(n)];
            for a in n.attributes() {
                if let Some(u) = a.namespace() {
                    if let Some(p) = n.lookup_prefix(u) {
                        if !p.is_empty() && p != "xml" {
                            used.push(p.to_string());
                        }
                    }
                }
            }
            for p in used {
                let uri = in_scope.get(&p).cloned().unwrap_or_default();
                if rendered.get(&p) != Some(&uri) && !(p.is_empty() && uri.is_empty() && rendered.get("").map(|u| u.is_empty()).unwrap_or(true)) {
                    decls.insert(p, uri);
                }
            }
        } else {
            for (p, u) in &in_scope {
                if rendered.get(p) != Some(u) {
                    decls.insert(p.clone(), u.clone());
                }
            }
            // A default namespace undone: xmlns="".
            if !in_scope.contains_key("") && rendered.get("").map(|u| !u.is_empty()).unwrap_or(false) {
                decls.insert(String::new(), String::new());
            }
            let _ = apex;
        }
        let name = qname(n, n.tag_name().namespace(), n.tag_name().name());
        self.out.push('<');
        self.out.push_str(&name);
        for (p, u) in &decls {
            if p.is_empty() {
                self.out.push_str(&format!(" xmlns=\"{}\"", esc_attr(u)));
            } else {
                self.out.push_str(&format!(" xmlns:{p}=\"{}\"", esc_attr(u)));
            }
        }
        let mut attrs: Vec<(String, String, String)> = n.attributes().map(|a| (a.namespace().unwrap_or("").to_string(), a.name().to_string(), a.value().to_string())).collect();
        attrs.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
        for (ns, local, v) in attrs {
            let q = if ns.is_empty() { local } else if ns == "http://www.w3.org/XML/1998/namespace" { format!("xml:{local}") } else { qname(n, Some(&ns), &local) };
            self.out.push_str(&format!(" {q}=\"{}\"", esc_attr(&v)));
        }
        self.out.push('>');
        let mut next = rendered.clone();
        for (p, u) in decls {
            next.insert(p, u);
        }
        for c in n.children() {
            match c.node_type() {
                NodeType::Element => self.element(c, &next, false),
                NodeType::Text => self.out.push_str(&esc_text(c.text().unwrap_or(""))),
                NodeType::Comment if self.comments => self.out.push_str(&format!("<!--{}-->", c.text().unwrap_or(""))),
                NodeType::PI => {
                    if let Some(pi) = c.pi() {
                        self.out.push_str(&format!("<?{}{}?>", pi.target, pi.value.map(|v| format!(" {v}")).unwrap_or_default()));
                    }
                }
                _ => {}
            }
        }
        self.out.push_str(&format!("</{name}>"));
    }
}

/// The canonical form of one element (and its subtree), skipping `skip` (the enveloped signature).
pub fn canonical(n: Node, method: &str, skip: Option<roxmltree::NodeId>) -> String {
    let exclusive = method.starts_with(EXC_C14N);
    let mut c = C { exclusive, comments: method.ends_with("#WithComments"), skip, out: String::new(), _p: Default::default() };
    // Inclusive c14n of a subtree starts with nothing rendered: every in-scope namespace shows at the apex.
    c.element(n, &BTreeMap::new(), true);
    c.out
}

fn id_of<'a, 'i>(n: Node<'a, 'i>) -> Option<&'a str> {
    n.attribute("Id").or(n.attribute("ID")).or(n.attribute("id")).or_else(|| n.attributes().find(|a| a.name() == "Id" && a.namespace().is_some()).map(|a| a.value()))
}

pub fn find_by_id<'a, 'i>(doc: &'a Document<'i>, id: &str) -> Option<Node<'a, 'i>> {
    doc.descendants().find(|n| n.is_element() && id_of(*n) == Some(id))
}

fn child<'a, 'i>(n: Node<'a, 'i>, local: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == local)
}

fn digest(method: &str, data: &[u8]) -> Vec<u8> {
    if method == SHA1 || method.ends_with("#sha1") {
        sha1::Sha1::digest(data).to_vec()
    } else {
        sha2::Sha256::digest(data).to_vec()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Checked {
    /// The signed element's id (Reference URI without #).
    pub reference: String,
    pub algorithm: String,
    /// The certificate in KeyInfo (DER), if any.
    pub certificate: Vec<u8>,
}

/// Checks every ds:Signature in the document: each reference's digest and the signature value with
/// the certificate in KeyInfo (or `key_pem` when there is none). Returns what was signed.
pub fn verify(xml: &str, key_pem: Option<&str>) -> Result<Vec<Checked>, String> {
    let doc = Document::parse(xml).map_err(|e| format!("not XML: {e}"))?;
    let sigs: Vec<Node> = doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "Signature" && n.tag_name().namespace() == Some(DSIG)).collect();
    if sigs.is_empty() {
        return Err("no signature (ds:Signature) in the document".into());
    }
    let mut out = vec![];
    for sig in sigs {
        let si = child(sig, "SignedInfo").ok_or("Signature without SignedInfo")?;
        let cm = child(si, "CanonicalizationMethod").and_then(|x| x.attribute("Algorithm")).unwrap_or(C14N).to_string();
        let sm = child(si, "SignatureMethod").and_then(|x| x.attribute("Algorithm")).unwrap_or(RSA_SHA1).to_string();
        let mut reference = String::new();
        for r in si.children().filter(|c| c.is_element() && c.tag_name().name() == "Reference") {
            let uri = r.attribute("URI").unwrap_or("");
            let target = if uri.is_empty() { Some(doc.root_element()) } else { find_by_id(&doc, uri.trim_start_matches('#')) };
            let target = target.ok_or_else(|| format!("Reference {uri}: no element with that Id"))?;
            let mut method = C14N.to_string();
            let mut enveloped = false;
            if let Some(ts) = child(r, "Transforms") {
                for t in ts.children().filter(|c| c.is_element()) {
                    match t.attribute("Algorithm").unwrap_or("") {
                        ENVELOPED => enveloped = true,
                        a => method = a.to_string(),
                    }
                }
            }
            let dm = child(r, "DigestMethod").and_then(|x| x.attribute("Algorithm")).unwrap_or(SHA1);
            let want = child(r, "DigestValue").and_then(|x| x.text()).map(|t| t.split_whitespace().collect::<String>()).unwrap_or_default();
            let canon = canonical(target, &method, if enveloped { Some(sig.id()) } else { None });
            let got = B64.encode(digest(dm, canon.as_bytes()));
            if got != want {
                return Err(format!("Reference {uri}: digest does not match (the signed content changed)"));
            }
            reference = uri.trim_start_matches('#').to_string();
        }
        let sv = child(sig, "SignatureValue").and_then(|x| x.text()).map(|t| t.split_whitespace().collect::<String>()).unwrap_or_default();
        let sv = B64.decode(sv).map_err(|_| "SignatureValue is not base64")?;
        let cert_b64 = sig.descendants().find(|n| n.is_element() && n.tag_name().name() == "X509Certificate").and_then(|n| n.text()).map(|t| t.split_whitespace().collect::<String>());
        let cert = cert_b64.and_then(|c| B64.decode(c).ok()).unwrap_or_default();
        let canon_si = canonical(si, &cm, None);
        let sha1 = sm.ends_with("rsa-sha1");
        let d = if sha1 { sha1::Sha1::digest(canon_si.as_bytes()).to_vec() } else { sha2::Sha256::digest(canon_si.as_bytes()).to_vec() };
        let ok = if !cert.is_empty() {
            verify_with_cert(&cert, sha1, &d, &sv)?
        } else if let Some(k) = key_pem {
            use rsa::pkcs8::DecodePublicKey;
            let pk = rsa::RsaPublicKey::from_public_key_pem(k).map_err(|e| e.to_string())?;
            pk.verify(if sha1 { rsa::Pkcs1v15Sign::new::<sha1::Sha1>() } else { rsa::Pkcs1v15Sign::new::<sha2::Sha256>() }, &d, &sv).is_ok()
        } else {
            return Err("the signature has no certificate (KeyInfo/X509Data) to check it with".into());
        };
        if !ok {
            return Err("SignatureValue does not match: signed with another key, or SignedInfo changed".into());
        }
        out.push(Checked { reference, algorithm: sm, certificate: cert });
    }
    Ok(out)
}

fn verify_with_cert(der: &[u8], sha1: bool, digest: &[u8], sig: &[u8]) -> Result<bool, String> {
    use rsa::pkcs1::DecodeRsaPublicKey;
    let (_, cert) = x509_parser::parse_x509_certificate(der).map_err(|e| format!("KeyInfo certificate: {e}"))?;
    let spki = cert.public_key();
    let pk = rsa::RsaPublicKey::from_pkcs1_der(&spki.subject_public_key.data).map_err(|e| format!("the certificate's key is not RSA: {e}"))?;
    Ok(pk.verify(if sha1 { rsa::Pkcs1v15Sign::new::<sha1::Sha1>() } else { rsa::Pkcs1v15Sign::new::<sha2::Sha256>() }, digest, sig).is_ok())
}

/// Where the Signature goes: after the signed element (NF-e, CT-e), or inside it after a child
/// (SAML: after saml:Issuer), or as its last child.
pub enum Place<'s> {
    AfterElement,
    InsideAfter(&'s str),
    InsideLast,
}

fn rsa_sign(key_pem: &str, sha1: bool, data: &[u8]) -> Result<Vec<u8>, String> {
    use rsa::pkcs8::DecodePrivateKey;
    let k = rsa::RsaPrivateKey::from_pkcs8_pem(key_pem).or_else(|_| {
        use rsa::pkcs1::DecodeRsaPrivateKey;
        rsa::RsaPrivateKey::from_pkcs1_pem(key_pem)
    }).map_err(|e| format!("private key: {e}"))?;
    let d = if sha1 { sha1::Sha1::digest(data).to_vec() } else { sha2::Sha256::digest(data).to_vec() };
    k.sign(if sha1 { rsa::Pkcs1v15Sign::new::<sha1::Sha1>() } else { rsa::Pkcs1v15Sign::new::<sha2::Sha256>() }, &d).map_err(|e| e.to_string())
}

/// Signs the element with this Id: an enveloped signature with the certificate in KeyInfo.
/// `exclusive` picks Exclusive c14n (SAML) or Canonical XML 1.0 (NF-e, CT-e); `prefix` "ds" or ""
/// (the default namespace, as SEFAZ documents use).
pub fn sign(xml: &str, id: &str, key_pem: &str, cert_der: &[u8], sha1: bool, exclusive: bool, place: Place, prefix: &str) -> Result<String, String> {
    let method = if exclusive { EXC_C14N } else { C14N };
    let p = if prefix.is_empty() { String::new() } else { format!("{prefix}:") };
    let ns = if prefix.is_empty() { format!(" xmlns=\"{DSIG}\"") } else { format!(" xmlns:{prefix}=\"{DSIG}\"") };
    let (sm, dm) = if sha1 { (RSA_SHA1, SHA1) } else { (RSA_SHA256, SHA256) };
    // 1. Where it goes, in the text.
    let pos = {
        let doc = Document::parse(xml).map_err(|e| format!("not XML: {e}"))?;
        let target = find_by_id(&doc, id).ok_or_else(|| format!("no element with Id {id}"))?;
        match place {
            Place::AfterElement => target.range().end,
            Place::InsideLast => {
                let r = target.range();
                r.start + xml[r.start..r.end].rfind("</").ok_or("the element has no end tag")?
            }
            Place::InsideAfter(local) => child(target, local).ok_or_else(|| format!("no {local} inside {id}"))?.range().end,
        }
    };
    let placeholder = "STUNTMAN_DIGEST";
    let placeholder_sv = "STUNTMAN_SIGNATURE";
    let sig = format!("<{p}Signature{ns}><{p}SignedInfo><{p}CanonicalizationMethod Algorithm=\"{method}\"/><{p}SignatureMethod Algorithm=\"{sm}\"/><{p}Reference URI=\"#{id}\"><{p}Transforms><{p}Transform Algorithm=\"{ENVELOPED}\"/><{p}Transform Algorithm=\"{method}\"/></{p}Transforms><{p}DigestMethod Algorithm=\"{dm}\"/><{p}DigestValue>{placeholder}</{p}DigestValue></{p}Reference></{p}SignedInfo><{p}SignatureValue>{placeholder_sv}</{p}SignatureValue><{p}KeyInfo><{p}X509Data><{p}X509Certificate>{}</{p}X509Certificate></{p}X509Data></{p}KeyInfo></{p}Signature>", B64.encode(cert_der));
    let with = format!("{}{}{}", &xml[..pos], sig, &xml[pos..]);
    // 2. The digest of the element as it is in the final document (enveloped: without the signature).
    let dv = {
        let doc = Document::parse(&with).map_err(|e| e.to_string())?;
        let target = find_by_id(&doc, id).ok_or("lost the element")?;
        let s = doc.descendants().find(|n| n.is_element() && n.tag_name().name() == "Signature" && n.tag_name().namespace() == Some(DSIG) && n.text().is_none() && n.descendants().any(|d| d.text() == Some(placeholder))).ok_or("lost the signature")?;
        B64.encode(digest(dm, canonical(target, method, Some(s.id())).as_bytes()))
    };
    let with = with.replacen(placeholder, &dv, 1);
    // 3. SignedInfo, canonical in its place, signed.
    let sv = {
        let doc = Document::parse(&with).map_err(|e| e.to_string())?;
        let si = doc.descendants().find(|n| n.is_element() && n.tag_name().name() == "SignedInfo" && n.descendants().any(|d| d.text() == Some(dv.as_str()))).ok_or("lost SignedInfo")?;
        B64.encode(rsa_sign(key_pem, sha1, canonical(si, method, None).as_bytes())?)
    };
    Ok(with.replacen(placeholder_sv, &sv, 1))
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn canonical_forms() {
        let x = r#"<a:root xmlns:a="urn:a" xmlns:b="urn:b" xmlns="urn:d"><a:x b:z="2" y="1" >t &amp; &lt;</a:x><e/></a:root>"#;
        let d = Document::parse(x).unwrap();
        let x1 = d.root_element().first_element_child().unwrap();
        assert_eq!(canonical(x1, EXC_C14N, None), r#"<a:x xmlns:a="urn:a" xmlns:b="urn:b" y="1" b:z="2">t &amp; &lt;</a:x>"#);
        assert_eq!(canonical(x1, C14N, None), r#"<a:x xmlns="urn:d" xmlns:a="urn:a" xmlns:b="urn:b" y="1" b:z="2">t &amp; &lt;</a:x>"#);
        let e = d.root_element().last_element_child().unwrap();
        assert_eq!(canonical(e, EXC_C14N, None), r#"<e xmlns="urn:d"></e>"#);
    }

    #[test]
    fn sign_then_verify() {
        let now = stuntman_core::util::now_ms() / 1000;
        let (key_pem, der) = crate::ca::get().icp("Acme Transportes Ltda", Some("11.222.333/0001-81"), None, now - 86_400, now + 86_400 * 365, false).unwrap();
        let nfe = r#"<NFe xmlns="http://www.portalfiscal.inf.br/nfe"><infNFe Id="NFe3526" versao="4.00"><ide><cUF>35</cUF></ide></infNFe></NFe>"#;
        let s = sign(nfe, "NFe3526", &key_pem, &der, true, false, Place::AfterElement, "").unwrap();
        let ok = verify(&s, None).unwrap();
        assert_eq!(ok[0].reference, "NFe3526");
        let tampered = s.replace("<cUF>35</cUF>", "<cUF>33</cUF>");
        assert!(verify(&tampered, None).unwrap_err().contains("digest"));
        let saml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" ID="_r"><saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a1" Version="2.0"><saml:Issuer>http://idp</saml:Issuer><saml:Subject><saml:NameID>ana@acme.example</saml:NameID></saml:Subject></saml:Assertion></samlp:Response>"#;
        let s = sign(saml, "_a1", &key_pem, &der, false, true, Place::InsideAfter("Issuer"), "ds").unwrap();
        assert!(s.contains("</saml:Issuer><ds:Signature"));
        assert_eq!(verify(&s, None).unwrap()[0].algorithm, RSA_SHA256);
    }
}
