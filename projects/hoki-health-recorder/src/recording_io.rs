//! Read only complete gzip members committed by the durable checkpoint.
use crate::Result;
use serde_json::Value;
use std::{fs::File,io::{Read,Cursor},path::Path};
pub fn segment(root:&Path, cp:&Value, index:u64)->Result<Cursor<Vec<u8>>> {
    let compressed=cp["compression"]=="gzip";
    let suffix=if compressed {"bin.gz"} else {"bin"};
    let file=File::open(root.join(format!("events-{index:06}.{suffix}")))?;
    let current=cp["segment"].as_u64().ok_or("missing segment")?;
    let physical=if index==current && compressed {cp["compressed_segment_bytes"].as_u64().ok_or("missing gzip boundary")?} else {file.metadata()?.len()};
    if physical>file.metadata()?.len(){return Err("truncated physical segment".into());}
    let mut data=Vec::new();
    // Protocol segments are at most64MiB, including decompressed data.
    if compressed {flate2::read::MultiGzDecoder::new(file.take(physical)).take(64*1024*1024+1).read_to_end(&mut data)?;}
    else {file.take(physical).take(64*1024*1024+1).read_to_end(&mut data)?;}
    if data.len()>64*1024*1024{return Err("oversize decoded segment".into());}
    let logical=if index==current {cp["segment_bytes"].as_u64().ok_or("missing logical boundary")? as usize} else {data.len()};
    if logical>data.len() || logical<16 || (logical-16)%88!=0 || (compressed && logical!=data.len()) {return Err("invalid decoded segment boundary".into());}
    data.truncate(logical);
    if &data[..16]!=b"HOKISEN1\x58\0\0\0\x01\0\0\0" {return Err("invalid record header".into());}
    Ok(Cursor::new(data))
}

#[cfg(test)] mod tests {
    use super::*;
    use std::io::Write;
    use flate2::{write::GzEncoder,Compression};
    fn member(bytes:&[u8])->Vec<u8>{let mut w=GzEncoder::new(Vec::new(),Compression::fast());w.write_all(bytes).unwrap();w.finish().unwrap()}
    #[test] fn reads_committed_members_and_ignores_interrupted_tail() {
        let root=std::env::temp_dir().join(format!("hoki-gzip-reader-{}",std::process::id()));std::fs::create_dir_all(&root).unwrap();
        let mut first=b"HOKISEN1\x58\0\0\0\x01\0\0\0".to_vec();first.extend([0u8;88]);
        let mut bytes=member(&first);bytes.extend(member(&[0u8;88]));let end=bytes.len();
        bytes.extend([0x1f,0x8b,8,0]);
        let path=root.join("events-000000.bin.gz");std::fs::write(&path,&bytes).unwrap();
        let cp=serde_json::json!({"compression":"gzip","segment":0,"segment_bytes":192,"compressed_segment_bytes":end});
        assert_eq!(segment(&root,&cp,0).unwrap().get_ref().len(),192);
        bytes[15]^=0xff;std::fs::write(&path,&bytes).unwrap();assert!(segment(&root,&cp,0).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
