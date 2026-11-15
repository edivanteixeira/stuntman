//! The Kafka wire format: big-endian primitives, strings, arrays, varints, and record batches
//! (magic 2) with their CRC-32C.

use std::io::Read;

#[derive(Default)]
pub struct W(pub Vec<u8>);

impl W {
    pub fn i8(&mut self, v: i8) -> &mut Self { self.0.push(v as u8); self }
    pub fn i16(&mut self, v: i16) -> &mut Self { self.0.extend(v.to_be_bytes()); self }
    pub fn i32(&mut self, v: i32) -> &mut Self { self.0.extend(v.to_be_bytes()); self }
    pub fn i64(&mut self, v: i64) -> &mut Self { self.0.extend(v.to_be_bytes()); self }
    pub fn u32(&mut self, v: u32) -> &mut Self { self.0.extend(v.to_be_bytes()); self }
    pub fn bool(&mut self, v: bool) -> &mut Self { self.0.push(v as u8); self }
    pub fn str(&mut self, s: &str) -> &mut Self { self.i16(s.len() as i16); self.0.extend(s.as_bytes()); self }
    pub fn nstr(&mut self, s: Option<&str>) -> &mut Self { match s { Some(x) => self.str(x), None => self.i16(-1) } }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self { self.i32(b.len() as i32); self.0.extend(b); self }
    pub fn nbytes(&mut self, b: Option<&[u8]>) -> &mut Self { match b { Some(x) => self.bytes(x), None => self.i32(-1) } }
    pub fn arr<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) -> &mut Self {
        self.i32(items.len() as i32);
        for x in items { f(self, x); }
        self
    }
    pub fn varint(&mut self, v: i64) -> &mut Self {
        let mut u = ((v << 1) ^ (v >> 63)) as u64;
        loop {
            let b = (u & 0x7f) as u8;
            u >>= 7;
            if u == 0 { self.0.push(b); break } else { self.0.push(b | 0x80) }
        }
        self
    }
}

pub struct R<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

pub type Res<T> = Result<T, String>;

impl<'a> R<'a> {
    pub fn new(b: &'a [u8]) -> Self { R { b, p: 0 } }
    fn take(&mut self, n: usize) -> Res<&'a [u8]> {
        if self.p + n > self.b.len() { return Err("short message".into()) }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    pub fn left(&self) -> usize { self.b.len() - self.p }
    pub fn i8(&mut self) -> Res<i8> { Ok(self.take(1)?[0] as i8) }
    pub fn i16(&mut self) -> Res<i16> { Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap())) }
    pub fn i32(&mut self) -> Res<i32> { Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap())) }
    pub fn i64(&mut self) -> Res<i64> { Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap())) }
    pub fn u32(&mut self) -> Res<u32> { Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap())) }
    pub fn str(&mut self) -> Res<String> { let n = self.i16()?; if n < 0 { return Ok(String::new()) } Ok(String::from_utf8_lossy(self.take(n as usize)?).to_string()) }
    pub fn nstr(&mut self) -> Res<Option<String>> { let n = self.i16()?; if n < 0 { return Ok(None) } Ok(Some(String::from_utf8_lossy(self.take(n as usize)?).to_string())) }
    pub fn bytes(&mut self) -> Res<Option<&'a [u8]>> { let n = self.i32()?; if n < 0 { return Ok(None) } Ok(Some(self.take(n as usize)?)) }
    pub fn count(&mut self) -> Res<i32> { self.i32() }
    pub fn varint(&mut self) -> Res<i64> {
        let mut u: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.take(1)?[0];
            u |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 { break }
            shift += 7;
            if shift > 63 { return Err("bad varint".into()) }
        }
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }
    pub fn vbytes(&mut self) -> Res<Option<&'a [u8]>> { let n = self.varint()?; if n < 0 { return Ok(None) } Ok(Some(self.take(n as usize)?)) }
}

/// CRC-32C (Castagnoli), as record batches use.
pub fn crc32c(data: &[u8]) -> u32 {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, x) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 { c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 } }
            *x = c;
        }
        t
    });
    let mut c = !0u32;
    for b in data { c = t[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8) }
    !c
}

#[derive(Clone, Debug, Default)]
pub struct Rec {
    pub offset: i64,
    pub ts: i64,
    pub key: Option<Vec<u8>>,
    pub value: Option<Vec<u8>>,
    pub headers: Vec<(String, Option<Vec<u8>>)>,
}

/// The records of one or more batches (a Produce partition's records). Err names why.
pub fn read_batches(b: &[u8]) -> Res<Vec<Rec>> {
    let mut out = vec![];
    let mut r = R::new(b);
    while r.left() >= 61 {
        let _base = r.i64()?;
        let len = r.i32()? as usize;
        let start = r.p;
        let _ple = r.i32()?;
        let magic = r.i8()?;
        if magic != 2 { return Err(format!("record batch magic {magic}: only v2 (Kafka 0.11+) is read")) }
        let _crc = r.u32()?;
        let attrs = r.i16()?;
        let _lod = r.i32()?;
        let first_ts = r.i64()?;
        let _max_ts = r.i64()?;
        let _pid = r.i64()?;
        let _pe = r.i16()?;
        let _bs = r.i32()?;
        let n = r.i32()?;
        let rest = &b[r.p..start + len];
        r.p = start + len;
        let data: Vec<u8> = match attrs & 7 {
            0 => rest.to_vec(),
            1 => { let mut o = vec![]; flate2::read::GzDecoder::new(rest).read_to_end(&mut o).map_err(|e| format!("gzip: {e}"))?; o }
            c => return Err(format!("compression {} is not supported (use none or gzip)", ["none", "gzip", "snappy", "lz4", "zstd"].get(c as usize).unwrap_or(&"?"))),
        };
        let mut rr = R::new(&data);
        for _ in 0..n {
            let _len = rr.varint()?;
            let _a = rr.i8()?;
            let tsd = rr.varint()?;
            let _od = rr.varint()?;
            let key = rr.vbytes()?.map(|x| x.to_vec());
            let value = rr.vbytes()?.map(|x| x.to_vec());
            let hn = rr.varint()?;
            let mut headers = vec![];
            for _ in 0..hn.max(0) {
                let k = rr.vbytes()?.map(|x| String::from_utf8_lossy(x).to_string()).unwrap_or_default();
                let v = rr.vbytes()?.map(|x| x.to_vec());
                headers.push((k, v));
            }
            out.push(Rec { offset: 0, ts: first_ts + tsd, key, value, headers });
        }
    }
    Ok(out)
}

/// One batch (magic 2, no compression) for these records, whose offsets follow each other.
pub fn write_batch(recs: &[Rec]) -> Vec<u8> {
    if recs.is_empty() { return vec![] }
    let base = recs[0].offset;
    let first_ts = recs.iter().map(|r| r.ts).min().unwrap_or(0);
    let max_ts = recs.iter().map(|r| r.ts).max().unwrap_or(0);
    let mut body = W::default();
    for r in recs {
        let mut x = W::default();
        x.i8(0).varint(r.ts - first_ts).varint(r.offset - base);
        match &r.key { Some(k) => { x.varint(k.len() as i64); x.0.extend(k); } None => { x.varint(-1); } }
        match &r.value { Some(v) => { x.varint(v.len() as i64); x.0.extend(v); } None => { x.varint(-1); } }
        x.varint(r.headers.len() as i64);
        for (k, v) in &r.headers {
            x.varint(k.len() as i64);
            x.0.extend(k.as_bytes());
            match v { Some(v) => { x.varint(v.len() as i64); x.0.extend(v); } None => { x.varint(-1); } }
        }
        body.varint(x.0.len() as i64);
        body.0.extend(x.0);
    }
    let mut after_crc = W::default();
    after_crc.i16(0).i32((recs.last().unwrap().offset - base) as i32).i64(first_ts).i64(max_ts).i64(-1).i16(-1).i32(-1).i32(recs.len() as i32);
    after_crc.0.extend(body.0);
    let crc = crc32c(&after_crc.0);
    let mut out = W::default();
    out.i64(base).i32((4 + 1 + 4 + after_crc.0.len()) as i32).i32(0).i8(2).u32(crc);
    out.0.extend(after_crc.0);
    out.0
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn batches_round_trip() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        let recs = vec![Rec { offset: 7, ts: 1_700_000_000_000, key: Some(b"k".to_vec()), value: Some(b"{\"a\":1}".to_vec()), headers: vec![("trace".into(), Some(b"x".to_vec()))] },
            Rec { offset: 8, ts: 1_700_000_000_005, key: None, value: Some(b"v2".to_vec()), headers: vec![] }];
        let b = write_batch(&recs);
        let back = read_batches(&b).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].value.as_deref(), Some(&b"{\"a\":1}"[..]));
        assert_eq!(back[1].ts, 1_700_000_000_005);
        let mut w = W::default();
        w.varint(-300).varint(5);
        let mut r = R::new(&w.0);
        assert_eq!((r.varint().unwrap(), r.varint().unwrap()), (-300, 5));
    }
}
