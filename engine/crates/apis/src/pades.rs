//! PAdES (ETSI.CAdES.detached) written by hand: an incremental update appends the signature
//! dictionary (a /Contents placeholder), its field and widget (the PNG, or invisible), the page with
//! the widget in /Annots and the AcroForm. The CMS SignedData is built here too and goes into the
//! placeholder once the browser has signed the attributes.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, Writer};
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, X509Certificate};

pub const CM_TO_PT: f64 = 1.0 / 0.0352778;
/// Room for the CMS in /Contents (bytes): the signature, attributes and a few certificates.
const RESERVED: usize = 16384;
const BYTE_RANGE: &str = "[0 0000000000 0000000000 0000000000]";

#[derive(Clone, Debug, Default)]
pub struct Visual {
    /// 1 is the first page; -1 the last.
    pub page: i64,
    /// PDF points.
    pub left: f64,
    pub bottom: f64,
    pub width: f64,
    pub height: f64,
    pub png: Option<Vec<u8>>,
}

pub struct Prepared {
    pub pdf: Vec<u8>,
    /// The hex digits of /Contents, [start, end).
    pub contents: (usize, usize),
    /// SHA-256 of the bytes the /ByteRange covers.
    pub digest: Vec<u8>,
    pub page: u32,
    pub rect: [f64; 4],
}

fn ser(o: &Object) -> Vec<u8> {
    let mut v = vec![];
    let _ = Writer::write_object(&mut v, o);
    v
}

fn real(x: f64) -> Object {
    Object::Real(((x * 100.0).round() / 100.0) as f32)
}

/// PNG → (RGB, alpha, width, height), 8 bits.
fn decode_png(b: &[u8]) -> Result<(Vec<u8>, Vec<u8>, u32, u32), String> {
    let mut d = png::Decoder::new(b);
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().map_err(|e| format!("image: {e}"))?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).map_err(|e| format!("image: {e}"))?;
    let px = &buf[..info.buffer_size()];
    let (mut rgb, mut alpha) = (vec![], vec![]);
    let n = info.color_type.samples();
    for p in px.chunks(n) {
        match n {
            1 => rgb.extend([p[0]; 3]),
            2 => rgb.extend([p[0]; 3]),
            _ => rgb.extend(&p[..3]),
        }
        alpha.push(if n == 2 || n == 4 { p[n - 1] } else { 255 });
    }
    Ok((rgb, alpha, info.width, info.height))
}

fn deflate(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
    let _ = e.write_all(b);
    e.finish().unwrap_or_default()
}

fn image(rgb: Vec<u8>, w: u32, h: u32, gray: bool, smask: Option<ObjectId>) -> Object {
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"XObject".to_vec()));
    d.set("Subtype", Object::Name(b"Image".to_vec()));
    d.set("Width", w as i64);
    d.set("Height", h as i64);
    d.set("ColorSpace", Object::Name(if gray { b"DeviceGray".to_vec() } else { b"DeviceRGB".to_vec() }));
    d.set("BitsPerComponent", 8);
    d.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    if let Some(m) = smask {
        d.set("SMask", Object::Reference(m));
    }
    Object::Stream(Stream::new(d, deflate(&rgb)))
}

fn pdf_date(secs: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("D:{:04}{:02}{:02}{:02}{:02}{:02}+00'00'", t.year(), t.month() as u8, t.day(), t.hour(), t.minute(), t.second())
}

/// The PDF with the signature field and the /Contents placeholder, and the digest to sign.
pub fn prepare(pdf: &[u8], v: &Visual, field: &str, now_secs: i64) -> Result<Prepared, String> {
    let doc = Document::load_mem(pdf).map_err(|e| format!("pdfToSign is not a PDF Stuntman can read: {e}"))?;
    // ponytail: encrypted PDFs are refused; rewriting their objects needs the key.
    if doc.is_encrypted() || doc.trailer.has(b"Encrypt") {
        return Err("pdfToSign is encrypted; Stuntman signs only unencrypted PDFs".into());
    }
    let pages = doc.get_pages();
    let n = if v.page < 0 { pages.len() as i64 + 1 + v.page } else { v.page.max(1) };
    let page_id = *pages.get(&(n as u32)).ok_or_else(|| format!("Page {} is not in the PDF ({} pages)", v.page, pages.len()))?;
    let root_id = doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|_| "The PDF has no /Root")?;
    let mut next = (doc.trailer.get(b"Size").and_then(Object::as_i64).unwrap_or(0) as u32).max(doc.max_id + 1);
    let mut new_id = || {
        next += 1;
        (next - 1, 0u16)
    };
    let (sig_id, widget_id, ap_id) = (new_id(), new_id(), new_id());
    let mut objs: Vec<(ObjectId, Vec<u8>)> = vec![];

    // Widget and appearance: the image fitted in the rectangle, or nothing (invisible).
    let visible = v.width > 0.0 && v.height > 0.0;
    let rect = if visible { [v.left, v.bottom, v.left + v.width, v.bottom + v.height] } else { [0.0; 4] };
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    let mut res = Dictionary::new();
    let mut content = String::new();
    if let (true, Some(png)) = (visible, &v.png) {
        let (rgb, alpha, iw, ih) = decode_png(png)?;
        let (img_id, mask_id) = (new_id(), new_id());
        objs.push((mask_id, ser(&image(alpha, iw, ih, true, None))));
        objs.push((img_id, ser(&image(rgb, iw, ih, false, Some(mask_id)))));
        let s = (w / iw as f64).min(h / ih as f64);
        let (dw, dh) = (iw as f64 * s, ih as f64 * s);
        content = format!("q {dw:.2} 0 0 {dh:.2} {:.2} {:.2} cm /Img Do Q", (w - dw) / 2.0, (h - dh) / 2.0);
        let mut xo = Dictionary::new();
        xo.set("Img", Object::Reference(img_id));
        res.set("XObject", xo);
    }
    let mut ap = Dictionary::new();
    ap.set("Type", Object::Name(b"XObject".to_vec()));
    ap.set("Subtype", Object::Name(b"Form".to_vec()));
    ap.set("BBox", vec![real(0.0), real(0.0), real(w), real(h)]);
    ap.set("Resources", res);
    objs.push((ap_id, ser(&Object::Stream(Stream::new(ap, content.into_bytes())))));
    let mut apd = Dictionary::new();
    apd.set("N", Object::Reference(ap_id));
    let mut wd = Dictionary::new();
    wd.set("Type", Object::Name(b"Annot".to_vec()));
    wd.set("Subtype", Object::Name(b"Widget".to_vec()));
    wd.set("FT", Object::Name(b"Sig".to_vec()));
    wd.set("T", Object::String(field.as_bytes().to_vec(), StringFormat::Literal));
    wd.set("V", Object::Reference(sig_id));
    wd.set("F", 132);
    wd.set("Rect", rect.iter().map(|x| real(*x)).collect::<Vec<_>>());
    wd.set("P", Object::Reference(page_id));
    wd.set("AP", apd);
    objs.push((widget_id, ser(&Object::Dictionary(wd))));

    // The page gets the widget in /Annots (an indirect array is rewritten in place).
    let mut page = doc.get_dictionary(page_id).map_err(|e| e.to_string())?.clone();
    match page.get(b"Annots") {
        Ok(Object::Reference(aid)) => {
            let mut a = doc.get_object(*aid).and_then(Object::as_array).cloned().unwrap_or_default();
            a.push(Object::Reference(widget_id));
            objs.push((*aid, ser(&Object::Array(a))));
        }
        got => {
            let mut a = got.ok().and_then(|o| o.as_array().ok()).cloned().unwrap_or_default();
            a.push(Object::Reference(widget_id));
            page.set("Annots", a);
            objs.push((page_id, ser(&Object::Dictionary(page))));
        }
    }
    // The AcroForm lists the field; SigFlags 3 = signatures exist, append only.
    let mut cat = doc.get_dictionary(root_id).map_err(|e| e.to_string())?.clone();
    let (form_id, mut form) = match cat.get(b"AcroForm").ok().and_then(|o| doc.dereference(o).ok()) {
        Some((id, o)) => (id, o.as_dict().cloned().unwrap_or_default()),
        None => (None, Dictionary::new()),
    };
    let mut fields = form.get(b"Fields").ok().and_then(|f| doc.dereference(f).ok()).and_then(|(_, o)| o.as_array().ok().cloned()).unwrap_or_default();
    fields.push(Object::Reference(widget_id));
    form.set("Fields", fields);
    form.set("SigFlags", 3);
    match form_id {
        Some(id) => objs.push((id, ser(&Object::Dictionary(form)))),
        None => {
            cat.set("AcroForm", form);
            objs.push((root_id, ser(&Object::Dictionary(cat))));
        }
    }
    let zeros = "0".repeat(RESERVED * 2);
    let sig = format!("<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /ETSI.CAdES.detached /M ({}) /ByteRange {BYTE_RANGE} /Contents <{zeros}> >>", pdf_date(now_secs));
    objs.push((sig_id, sig.into_bytes()));
    objs.sort_by_key(|o| o.0);

    // The update: objects, then a cross-reference section in the original's style (iText follows
    // /Prev from a classic table only into another table, so an xref-stream file gets a stream).
    if doc.xref_start == 0 {
        // ponytail: lopdf rebuilt a broken xref; there is no /Prev to point at. A full rewrite would fix it.
        return Err("pdfToSign has a broken cross-reference table".into());
    }
    let stream_xref = !pdf.get(doc.xref_start..).unwrap_or_default().trim_ascii_start().starts_with(b"xref");
    let xref_id = if stream_xref { Some(new_id()) } else { None };
    let mut out = pdf.to_vec();
    if !out.ends_with(b"\n") && !out.ends_with(b"\r") {
        out.push(b'\n');
    }
    let mut offsets = vec![];
    let mut sig_at = 0;
    for (id, body) in &objs {
        offsets.push((*id, out.len()));
        out.extend(format!("{} {} obj\n", id.0, id.1).as_bytes());
        if *id == sig_id {
            sig_at = out.len();
        }
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref_at = out.len();
    let mut tr = Dictionary::new();
    tr.set("Size", next as i64);
    tr.set("Root", Object::Reference(root_id));
    tr.set("Prev", doc.xref_start as i64);
    for k in [&b"Info"[..], b"ID"] {
        if let Ok(o) = doc.trailer.get(k) {
            tr.set(k, o.clone());
        }
    }
    match xref_id {
        Some(xid) => {
            offsets.push((xid, xref_at));
            let mut rows = vec![];
            for (id, at) in &offsets {
                rows.push(1u8);
                rows.extend((*at as u32).to_be_bytes());
                rows.extend(id.1.to_be_bytes());
            }
            tr.set("Type", Object::Name(b"XRef".to_vec()));
            tr.set("W", vec![Object::Integer(1), Object::Integer(4), Object::Integer(2)]);
            tr.set("Index", offsets.iter().flat_map(|(id, _)| [Object::Integer(id.0 as i64), Object::Integer(1)]).collect::<Vec<_>>());
            out.extend(format!("{} 0 obj\n", xid.0).as_bytes());
            out.extend(ser(&Object::Stream(Stream::new(tr, rows))));
            out.extend(b"\nendobj\n");
        }
        None => {
            out.extend(b"xref\n");
            for (id, at) in &offsets {
                out.extend(format!("{} 1\n{at:010} {:05} n \n", id.0, id.1).as_bytes());
            }
            out.extend(b"trailer\n");
            out.extend(ser(&Object::Dictionary(tr)));
        }
    }
    out.extend(format!("\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());

    // /ByteRange around /Contents <…>, padded to the placeholder's width.
    let find = |needle: &[u8]| out[sig_at..].windows(needle.len()).position(|w| w == needle).map(|p| p + sig_at);
    let br = find(BYTE_RANGE.as_bytes()).ok_or("ByteRange placeholder")?;
    let a = find(b"/Contents <").ok_or("Contents placeholder")? + 10;
    let b = a + zeros.len() + 2;
    let range = format!("[0 {a} {b} {}]", out.len() - b);
    out[br..br + BYTE_RANGE.len()].copy_from_slice(format!("{range:<width$}", width = BYTE_RANGE.len()).as_bytes());
    let mut hsh = Sha256::new();
    hsh.update(&out[..a]);
    hsh.update(&out[b..]);
    Ok(Prepared { digest: hsh.finalize().to_vec(), pdf: out, contents: (a + 1, b - 1), page: n as u32, rect })
}

/// The signed PDF: the CMS in the placeholder.
pub fn embed(pdf: &[u8], contents: (usize, usize), cms: &[u8]) -> Result<Vec<u8>, String> {
    let hex = hex::encode_upper(cms);
    if hex.len() > contents.1 - contents.0 {
        return Err(format!("The signature ({} bytes) does not fit the reserved {} bytes", cms.len(), (contents.1 - contents.0) / 2));
    }
    let mut out = pdf.to_vec();
    out[contents.0..contents.0 + hex.len()].copy_from_slice(hex.as_bytes());
    Ok(out)
}

// ---------------------------------------------------------------- CMS (DER by hand)

fn tlv(tag: u8, c: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = c.len();
    if n < 128 {
        v.push(n as u8);
    } else {
        let b = n.to_be_bytes();
        let b = &b[b.iter().position(|x| *x != 0).unwrap_or(7)..];
        v.push(0x80 | b.len() as u8);
        v.extend(b);
    }
    v.extend(c);
    v
}

fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn oid(s: &str) -> Vec<u8> {
    let a: Vec<u64> = s.split('.').filter_map(|x| x.parse().ok()).collect();
    let mut c = vec![(a[0] * 40 + a[1]) as u8];
    for &x in &a[2..] {
        let mut b = vec![(x & 0x7f) as u8];
        let mut y = x >> 7;
        while y > 0 {
            b.push(0x80 | (y & 0x7f) as u8);
            y >>= 7;
        }
        b.reverse();
        c.extend(b);
    }
    tlv(0x06, &c)
}

const SHA256: &str = "2.16.840.1.101.3.4.2.1";
const DATA: &str = "1.2.840.113549.1.7.1";

pub struct CertInfo {
    pub issuer: Vec<u8>,
    pub subject: Vec<u8>,
    /// INTEGER content bytes.
    pub serial: Vec<u8>,
    pub spki: Vec<u8>,
}

pub fn cert_info(der: &[u8]) -> Result<CertInfo, String> {
    let (_, c) = X509Certificate::from_der(der).map_err(|e| format!("certificate: {e}"))?;
    Ok(CertInfo { issuer: c.tbs_certificate.issuer.as_raw().to_vec(), subject: c.tbs_certificate.subject.as_raw().to_vec(), serial: c.tbs_certificate.raw_serial().to_vec(), spki: c.public_key().raw.to_vec() })
}

/// The signed attributes (DER SET): contentType, messageDigest, signingCertificateV2.
pub fn signed_attrs(digest: &[u8], cert: &[u8]) -> Result<Vec<u8>, String> {
    let ci = cert_info(cert)?;
    let attr = |o: &str, v: Vec<u8>| seq(&[&oid(o), &tlv(0x31, &v)]);
    let issuer_serial = seq(&[&seq(&[&tlv(0xa4, &ci.issuer)]), &tlv(0x02, &ci.serial)]);
    let ess = seq(&[&seq(&[&seq(&[&tlv(0x04, &Sha256::digest(cert)), &issuer_serial])])]);
    let mut a = vec![attr("1.2.840.113549.1.9.3", oid(DATA)), attr("1.2.840.113549.1.9.4", tlv(0x04, digest)), attr("1.2.840.113549.1.9.16.2.47", ess)];
    a.sort(); // DER: SET OF in encoding order
    Ok(tlv(0x31, &a.concat()))
}

/// ContentInfo(SignedData), detached, with the signer certificate and its issuers.
pub fn cms(signed_attrs: &[u8], signature: &[u8], cert: &[u8], chain: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let ci = cert_info(cert)?;
    let alg = seq(&[&oid(SHA256)]);
    let certs: Vec<u8> = std::iter::once(cert.to_vec()).chain(chain.iter().cloned()).collect::<Vec<_>>().concat();
    let mut attrs = signed_attrs.to_vec();
    attrs[0] = 0xa0; // [0] IMPLICIT in the SignerInfo, the same bytes otherwise
    let signer = seq(&[&tlv(0x02, &[1]), &seq(&[&ci.issuer, &tlv(0x02, &ci.serial)]), &alg, &attrs,
        &seq(&[&oid("1.2.840.113549.1.1.1"), &[0x05, 0x00]]), &tlv(0x04, signature)]);
    let sd = seq(&[&tlv(0x02, &[1]), &tlv(0x31, &alg), &seq(&[&oid(DATA)]), &tlv(0xa0, &certs), &tlv(0x31, &signer)]);
    Ok(seq(&[&oid("1.2.840.113549.1.7.2"), &tlv(0xa0, &sd)]))
}

/// RSA PKCS#1 v1.5 SHA-256 over the signed attributes, with the certificate's key.
pub fn verify(cert: &[u8], data: &[u8], signature: &[u8]) -> bool {
    use rsa::pkcs8::DecodePublicKey;
    let Ok(ci) = cert_info(cert) else { return false };
    let Ok(k) = rsa::RsaPublicKey::from_public_key_der(&ci.spki) else { return false };
    k.verify(rsa::Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(data), signature).is_ok()
}

#[cfg(test)]
mod t {
    use super::*;

    const PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] >>\nendobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n186\n%%EOF\n";

    #[test]
    fn byte_range_covers_all_but_contents() {
        let v = Visual { page: 1, left: 100.0, bottom: 100.0, width: 0.0, height: 0.0, png: None };
        let p = prepare(PDF, &v, "Sig-1", 0).unwrap();
        assert_eq!(&p.pdf[..PDF.len()], PDF, "an incremental update keeps the original bytes");
        assert_eq!(p.pdf[p.contents.0 - 1], b'<');
        assert_eq!(p.pdf[p.contents.1], b'>');
        let d = Document::load_mem(&p.pdf).unwrap();
        let f = d.catalog().unwrap().get(b"AcroForm").unwrap().as_dict().unwrap();
        assert_eq!(f.get(b"Fields").unwrap().as_array().unwrap().len(), 1);
        let signed = embed(&p.pdf, p.contents, &[0x30, 0x00]).unwrap();
        assert_eq!(signed.len(), p.pdf.len());
        assert!(oid("1.2.840.113549.1.9.16.2.47").ends_with(&[0x10, 0x02, 0x2f]));
    }
}

