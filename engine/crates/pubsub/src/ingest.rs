//! Import topics: a topic with ingestion settings (Amazon Kinesis, Cloud Storage, Azure Event Hubs,
//! Amazon MSK, Confluent Cloud) gets messages from its source. Where Stuntman has the source, it
//! reads it for real: Cloud Storage from a storage account holding a container of the bucket's name
//! (text split at the delimiter, gzip/zstandard/bzip2/xz too; Avro and Pub/Sub Avro object container
//! files, codecs null, deflate, snappy, zstandard, bzip2 and xz), MSK and Confluent
//! Cloud from the Kafka topic of that name in Streams, from the earliest offset; Amazon Kinesis from
//! the Stuntman Kinesis stream the ARN names, from TRIM_HORIZON. Every source can be
//! fed with the pubsub_ingest tool. Ingested messages go through the topic's transforms and schema.
//!
//! The mapping (Google documents data and, for Pub/Sub Avro, attributes and
//! original_message_ordering_key; the rest is Stuntman's): Kafka value → data, key → ordering key,
//! headers → attributes; Kinesis data → data, partition key → ordering key; Event Hubs body → data,
//! partition key → ordering key, properties → attributes.

use serde_json::{json, Value};
use stuntman_core::{ApiError, Ctx};

use crate::{
    grpc,
    store::{self, Labels},
    v1::{ingestion_data_source_settings::{cloud_storage::InputFormat, Source}, PubsubMessage, Topic},
};

/// One record as the source holds it.
#[derive(Default, Clone, Debug)]
pub struct Rec {
    pub data: Vec<u8>,
    pub key: String,
    pub attrs: Labels,
}

pub fn source(t: &Topic) -> Option<&Source> {
    t.ingestion_data_source_settings.as_ref()?.source.as_ref()
}

/// (kind, what it reads, state) for the screen and the tools.
pub fn describe(t: &Topic) -> Option<Value> {
    let (kind, from, state) = match source(t)? {
        Source::AwsKinesis(k) => ("aws_kinesis", k.stream_arn.clone(), crate::v1::ingestion_data_source_settings::aws_kinesis::State::try_from(k.state).map(|s| s.as_str_name()).unwrap_or("")),
        Source::CloudStorage(c) => ("cloud_storage", format!("gs://{}{}", c.bucket, if c.match_glob.is_empty() { String::new() } else { format!(" · {}", c.match_glob) }),
            crate::v1::ingestion_data_source_settings::cloud_storage::State::try_from(c.state).map(|s| s.as_str_name()).unwrap_or("")),
        Source::AzureEventHubs(e) => ("azure_event_hubs", format!("{}/{}", e.namespace, e.event_hub), crate::v1::ingestion_data_source_settings::azure_event_hubs::State::try_from(e.state).map(|s| s.as_str_name()).unwrap_or("")),
        Source::AwsMsk(m) => ("aws_msk", m.topic.clone(), crate::v1::ingestion_data_source_settings::aws_msk::State::try_from(m.state).map(|s| s.as_str_name()).unwrap_or("")),
        Source::ConfluentCloud(c) => ("confluent_cloud", c.topic.clone(), crate::v1::ingestion_data_source_settings::confluent_cloud::State::try_from(c.state).map(|s| s.as_str_name()).unwrap_or("")),
    };
    Some(json!({"kind": kind, "from": from, "state": state}))
}

/// The source's state as Stuntman sees it: ACTIVE, or BUCKET_NOT_FOUND / TOPIC_NOT_FOUND /
/// STREAM_NOT_FOUND when the backend has no such bucket, Kafka topic or Kinesis stream
/// (pubsub_ingest still feeds it).
pub async fn set_state(ctx: &Ctx, t: &mut Topic) -> Result<bool, ApiError> {
    let Some(src) = t.ingestion_data_source_settings.as_mut().and_then(|s| s.source.as_mut()) else { return Ok(false) };
    let (slot, want) = match src {
        Source::AwsKinesis(k) => { let found = kinesis_stream(ctx, &k.stream_arn).await?.is_some(); (&mut k.state, if found { 1 } else { 4 }) }
        Source::AzureEventHubs(e) => (&mut e.state, 1),
        Source::CloudStorage(c) => { let found = bucket_account(ctx, &c.bucket).is_some(); (&mut c.state, if found { 1 } else { 4 }) }
        Source::AwsMsk(m) => { let found = stuntman_streams::store::topic(ctx, &m.topic).await?.is_some(); (&mut m.state, if found { 1 } else { 5 }) }
        Source::ConfluentCloud(c) => { let found = stuntman_streams::store::topic(ctx, &c.topic).await?.is_some(); (&mut c.state, if found { 1 } else { 6 }) }
    };
    let changed = *slot != want;
    *slot = want;
    Ok(changed)
}

/// The Stuntman Kinesis stream an ARN (or a bare name) names, with its shard ids, parents first.
async fn kinesis_stream(ctx: &Ctx, arn: &str) -> Result<Option<(String, Vec<String>)>, ApiError> {
    let name = stuntman_kinesis::store::stream_name(arn);
    ctx.db.call(move |cn| Ok(stuntman_kinesis::store::load(cn, &name).ok().map(|s| (s.name, s.shards.into_iter().map(|x| x.id).collect())))).await
}

fn bucket_account(ctx: &Ctx, bucket: &str) -> Option<String> {
    stuntman_storage::config::list(ctx).into_iter().find(|a| a.containers.contains_key(bucket)).map(|a| a.account)
}

/// Publishes records as the topic's source would.
pub async fn feed(ctx: &Ctx, t: &Topic, recs: Vec<Rec>) -> Result<Vec<String>, ApiError> {
    if recs.is_empty() { return Ok(vec![]) }
    let kind = describe(t).map(|d| d["kind"].as_str().unwrap_or("").to_string()).unwrap_or_default();
    let msgs = recs.into_iter().map(|r| PubsubMessage { data: r.data, attributes: r.attrs.into_iter().collect(), ordering_key: r.key, ..Default::default() }).collect();
    grpc::accept(ctx, t, msgs, &format!("ingestion:{kind}")).await.map_err(|e| ApiError::bad(e.message().to_string()))
}

/// `*` within a path segment, `**` across segments, `?` one character.
pub fn glob(pat: &str, s: &str) -> bool {
    fn m(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) if p.get(1) == Some(&b'*') => (0..=s.len()).any(|i| m(&p[2..], &s[i..])),
            (Some(b'*'), _) => (0..=s.len()).take_while(|i| *i == 0 || s[i - 1] != b'/').any(|i| m(&p[1..], &s[i..])),
            (Some(b'?'), Some(c)) if *c != b'/' => m(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => m(&p[1..], &s[1..]),
            _ => false,
        }
    }
    pat.is_empty() || m(pat.as_bytes(), s.as_bytes())
}

fn zz(b: &[u8], p: &mut usize) -> Result<i64, String> {
    let (mut u, mut shift) = (0u64, 0);
    loop {
        let x = *b.get(*p).ok_or("truncated Avro file")?;
        *p += 1;
        u |= ((x & 0x7f) as u64) << shift;
        if x & 0x80 == 0 { return Ok(((u >> 1) as i64) ^ -((u & 1) as i64)) }
        shift += 7;
        if shift > 63 { return Err("bad varint".into()) }
    }
}

fn chunk<'a>(b: &'a [u8], p: &mut usize) -> Result<&'a [u8], String> {
    let n = zz(b, p)? as usize;
    let s = b.get(*p..*p + n).ok_or("truncated Avro file")?;
    *p += n;
    Ok(s)
}

/// Raw Snappy (the Avro snappy codec's blocks): the uncompressed length, then literals and copies.
pub fn unsnappy(b: &[u8]) -> Result<Vec<u8>, String> {
    let (mut n, mut shift, mut p) = (0usize, 0, 0);
    loop {
        let x = *b.get(p).ok_or("truncated snappy block")?;
        p += 1;
        n |= ((x & 0x7f) as usize) << shift;
        if x & 0x80 == 0 { break }
        shift += 7;
    }
    let mut out = Vec::with_capacity(n);
    let get = |p: usize, k: usize| -> Result<usize, String> { Ok(b.get(p..p + k).ok_or("truncated snappy block")?.iter().enumerate().map(|(i, x)| (*x as usize) << (8 * i)).sum()) };
    while p < b.len() {
        let tag = b[p];
        p += 1;
        let (len, off) = match tag & 3 {
            0 => {
                let mut len = (tag >> 2) as usize;
                if len >= 60 { let k = len - 59; len = get(p, k)?; p += k; }
                let lit = b.get(p..p + len + 1).ok_or("truncated snappy literal")?;
                out.extend(lit);
                p += len + 1;
                continue;
            }
            1 => { let off = (((tag >> 5) as usize) << 8) | get(p, 1)?; p += 1; (4 + ((tag >> 2) & 7) as usize, off) }
            2 => { let off = get(p, 2)?; p += 2; (1 + (tag >> 2) as usize, off) }
            _ => { let off = get(p, 4)?; p += 4; (1 + (tag >> 2) as usize, off) }
        };
        if off == 0 || off > out.len() { return Err("bad snappy copy offset".into()) }
        for _ in 0..len { out.push(out[out.len() - off]); }
    }
    if out.len() != n { return Err("snappy length mismatch".into()) }
    Ok(out)
}

/// Decompresses with a pure-Rust codec: gzip, deflate (raw), zstandard, bzip2, xz.
pub fn decompress(codec: &str, b: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut out = vec![];
    match codec {
        "gzip" => flate2::read::MultiGzDecoder::new(b).read_to_end(&mut out).map(|_| ()).map_err(|e| e.to_string()),
        "deflate" => flate2::read::DeflateDecoder::new(b).read_to_end(&mut out).map(|_| ()).map_err(|e| e.to_string()),
        "zstandard" => {
            // One or more frames, one after another.
            let mut src = b;
            while !src.is_empty() {
                let mut d = ruzstd::decoding::StreamingDecoder::new(&mut src).map_err(|e| format!("zstandard: {e}"))?;
                d.read_to_end(&mut out).map_err(|e| format!("zstandard: {e}"))?;
            }
            Ok(())
        }
        "bzip2" => bzip2_rs::DecoderReader::new(b).read_to_end(&mut out).map(|_| ()).map_err(|e| format!("bzip2: {e}")),
        "xz" => lzma_rs::xz_decompress(&mut std::io::BufReader::new(b), &mut out).map_err(|e| format!("xz: {e:?}")),
        c => Err(format!("codec {c} is not read")),
    }?;
    Ok(out)
}

/// The compression an object's first bytes show (gzip, zstandard, bzip2, xz).
pub fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x1f, 0x8b]) { Some("gzip") }
    else if b.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) { Some("zstandard") }
    else if b.starts_with(b"BZh") { Some("bzip2") }
    else if b.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) { Some("xz") }
    else { None }
}

/// The records of an Avro object container file: codecs null, deflate, snappy, zstandard, bzip2
/// and xz.
pub fn avro_records(b: &[u8]) -> Result<Vec<Value>, String> {
    if !b.starts_with(b"Obj\x01") { return Err("not an Avro object container file".into()) }
    let mut p = 4;
    let (mut schema, mut codec) = (None, "null".to_string());
    loop {
        let n = zz(b, &mut p)?;
        if n == 0 { break }
        if n < 0 { zz(b, &mut p)?; }
        for _ in 0..n.abs() {
            let k = String::from_utf8_lossy(chunk(b, &mut p)?).to_string();
            let v = chunk(b, &mut p)?;
            if k == "avro.schema" { schema = Some(serde_json::from_slice::<Value>(v).map_err(|e| e.to_string())?) }
            if k == "avro.codec" { codec = String::from_utf8_lossy(v).to_string() }
        }
    }
    if !["null", "deflate", "snappy", "zstandard", "bzip2", "xz"].contains(&codec.as_str()) { return Err(format!("Avro codec {codec} is not read")) }
    let schema = schema.ok_or("the Avro file has no schema")?;
    p += 16;
    let mut out = vec![];
    while p < b.len() {
        let n = zz(b, &mut p)? as usize;
        let raw = chunk(b, &mut p)?;
        let block = match codec.as_str() {
            "deflate" | "zstandard" | "bzip2" | "xz" => decompress(&codec, raw)?,
            "snappy" => {
                let (body, crc) = raw.split_at(raw.len().checked_sub(4).ok_or("snappy block without its CRC")?);
                let d = unsnappy(body)?;
                if crc32fast::hash(&d).to_be_bytes() != crc { return Err("snappy block CRC mismatch".into()) }
                d
            }
            _ => raw.to_vec(),
        };
        out.extend(stuntman_streams::avro::decode_seq(&schema, &block, n)?.0);
        p += 16;
    }
    Ok(out)
}

/// The records of one Cloud Storage object, by the topic's input format.
pub fn object_records(format: Option<&InputFormat>, data: &[u8]) -> Result<Vec<Rec>, String> {
    // A compressed object (gzip, zstandard, bzip2 or xz, known by its first bytes) is read decompressed.
    let unzipped;
    let data = match sniff(data) { Some(c) => { unzipped = decompress(c, data)?; unzipped.as_slice() } None => data };
    match format {
        Some(InputFormat::AvroFormat(_)) => Ok(avro_records(data)?.into_iter().map(|v| Rec { data: v.to_string().into_bytes(), ..Default::default() }).collect()),
        Some(InputFormat::PubsubAvroFormat(_)) => Ok(avro_records(data)?.into_iter().map(|v| {
            let mut attrs: Labels = v["attributes"].as_object().map(|o| o.iter().map(|(k, x)| (k.clone(), x.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default();
            if let Some(k) = v["ordering_key"].as_str().filter(|k| !k.is_empty()) { attrs.insert("original_message_ordering_key".into(), k.into()); }
            Rec { data: v["data"].as_str().unwrap_or_default().as_bytes().to_vec(), key: String::new(), attrs }
        }).collect()),
        other => {
            let d = match other { Some(InputFormat::TextFormat(t)) => t.delimiter.clone().filter(|x| !x.is_empty()).unwrap_or("\n".into()), _ => "\n".into() };
            let d = d.as_bytes().first().copied().unwrap_or(b'\n');
            Ok(data.split(|c| *c == d).filter(|x| !x.is_empty()).map(|x| Rec { data: x.to_vec(), ..Default::default() }).collect())
        }
    }
}

/// Reads what the backend has that the topic has not ingested yet.
pub async fn poll(ctx: &Ctx, t: &Topic) -> Result<(), ApiError> {
    let mut t = t.clone();
    if set_state(ctx, &mut t).await? { store::save_topic(ctx, &t).await?; }
    match source(&t).cloned() {
        Some(Source::CloudStorage(c)) => {
            let Some(acct) = bucket_account(ctx, &c.bucket) else { return Ok(()) };
            let min = c.minimum_object_create_time.as_ref().map(|x| x.seconds * 1000 + x.nanos as i64 / 1_000_000).unwrap_or(0);
            for b in stuntman_storage::store::list(ctx, &acct, &c.bucket, "").await? {
                if b.at < min || !glob(&c.match_glob, &b.path) { continue }
                let k = format!("gcs:{}", b.path);
                if store::ingest_mark(ctx, &t.name, &k).await? == Some(b.seq) { continue }
                store::set_ingest_mark(ctx, &t.name, &k, b.seq).await?;
                let Some(full) = stuntman_storage::store::get(ctx, &acct, &c.bucket, &b.path, true).await? else { continue };
                // An object that is not in the format is not ingested (Pub/Sub logs it).
                match object_records(c.input_format.as_ref(), &full.data) {
                    Ok(recs) => { let _ = feed(ctx, &t, recs).await; }
                    Err(e) => eprintln!("Pub/Sub ingestion: {} skipped: {e}", b.path),
                }
            }
        }
        Some(Source::AwsMsk(crate::v1::ingestion_data_source_settings::AwsMsk { topic: kt, .. })) | Some(Source::ConfluentCloud(crate::v1::ingestion_data_source_settings::ConfluentCloud { topic: kt, .. })) => {
            let Some(parts) = stuntman_streams::store::topic(ctx, &kt).await? else { return Ok(()) };
            for part in 0..parts {
                let k = format!("kafka:{kt}:{part}");
                let from = store::ingest_mark(ctx, &t.name, &k).await?.unwrap_or(0);
                let recs = stuntman_streams::store::read(ctx, &kt, part, from, 1 << 20).await?;
                let Some(last) = recs.last().map(|r| r.offset) else { continue };
                store::set_ingest_mark(ctx, &t.name, &k, last + 1).await?;
                let out = recs.into_iter().map(|r| Rec {
                    data: r.value.unwrap_or_default(),
                    key: r.key.map(|k| String::from_utf8_lossy(&k).to_string()).unwrap_or_default(),
                    attrs: r.headers.into_iter().map(|(k, v)| (k, v.map(|x| String::from_utf8_lossy(&x).to_string()).unwrap_or_default())).collect(),
                }).collect();
                feed(ctx, &t, out).await?;
            }
        }
        Some(Source::AwsKinesis(k)) => {
            // From TRIM_HORIZON at creation, shard by shard (parents before their children).
            let Some((stream, shards)) = kinesis_stream(ctx, &k.stream_arn).await? else { return Ok(()) };
            let now = stuntman_core::clock::now(ctx);
            for shard in shards {
                let mk = format!("kinesis:{stream}:{shard}");
                let from = store::ingest_mark(ctx, &t.name, &mk).await?.unwrap_or(0);
                let (s2, sh2) = (stream.clone(), shard.clone());
                let recs = ctx.db.call(move |cn| Ok(stuntman_kinesis::store::shard_records(cn, &s2, &sh2, from, now).unwrap_or_default())).await?;
                let Some(last) = recs.last().map(|r| r.0) else { continue };
                store::set_ingest_mark(ctx, &t.name, &mk, last + 1).await?;
                feed(ctx, &t, recs.into_iter().map(|(_, key, data)| Rec { data, key, ..Default::default() }).collect()).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn globs_text_and_avro_objects() {
        assert!(glob("**.txt", "shipments/2026/11/a.txt") && glob("shipments/*.csv", "shipments/a.csv") && !glob("shipments/*.csv", "shipments/x/a.csv") && glob("", "x"));
        let r = object_records(None, b"SHP-1 created\nSHP-2 created\n").unwrap();
        assert_eq!(r.iter().map(|x| String::from_utf8(x.data.clone()).unwrap()).collect::<Vec<_>>(), ["SHP-1 created", "SHP-2 created"]);
        // A file the Cloud Storage subscription writes reads back as Pub/Sub Avro.
        let s = crate::store::Sub { name: "projects/acme-stg/subscriptions/archive".into(), ..Default::default() };
        let g = crate::store::Got { msg: crate::store::Msg { id: 4, data: b"SHP-4 delivered".to_vec(), attributes: Labels::from([("region".into(), "us-east".into())]), ..Default::default() }, ack_id: String::new(), attempt: 1 };
        let f = crate::deliver::avro_file(&s, &[g], true, None);
        let r = object_records(Some(&InputFormat::PubsubAvroFormat(Default::default())), &f).unwrap();
        assert_eq!((r[0].data.clone(), r[0].attrs.get("region").cloned()), (b"SHP-4 delivered".to_vec(), Some("us-east".to_string())));
        // gzip text.
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
        gz.write_all(b"ORD-1\nORD-2\n").unwrap();
        assert_eq!(object_records(None, &gz.finish().unwrap()).unwrap().len(), 2);
        // Snappy: a literal "abcd" then a copy of 4 at offset 4 → "abcdabcd".
        assert_eq!(unsnappy(&[8, 0x0c, b'a', b'b', b'c', b'd', 0x01, 4]).unwrap(), b"abcdabcd");
        // zstandard round trip (the Compression transform's encoder).
        let z = ruzstd::encoding::compress_to_vec(&b"ORD-3\nORD-4\n"[..], ruzstd::encoding::CompressionLevel::Fastest);
        assert_eq!(sniff(&z), Some("zstandard"));
        assert_eq!(object_records(None, &z).unwrap().len(), 2);
    }
}
