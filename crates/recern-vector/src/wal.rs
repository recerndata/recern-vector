//! Checksummed, committed page after-images. An incomplete final frame is ignored.
use crate::{
    error::{Error, Result},
    storage,
};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
const MAGIC: &[u8; 8] = b"RVWAL\0\x01\0";
const PAGE: usize = 4096;
const HEADER: usize = 24;

pub(crate) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}
pub(crate) fn header(identity: u128) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend(identity.to_le_bytes());
    bytes
}
pub(crate) fn frame(old: &[u8], new: &[u8], sequence: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend(sequence.to_le_bytes());
    payload.extend((new.len() as u64).to_le_bytes());
    payload.extend(&old[old.len() - 4..]);
    for (page, bytes) in new.chunks(PAGE).enumerate() {
        let start = page * PAGE;
        if old.get(start..start + bytes.len()) != Some(bytes) {
            payload.extend((start as u64).to_le_bytes());
            payload.extend((bytes.len() as u32).to_le_bytes());
            payload.extend(bytes);
        }
    }
    let mut out = (payload.len() as u64).to_le_bytes().to_vec();
    out.extend(storage::crc32(&out).to_le_bytes());
    out.extend(&payload);
    out.extend(storage::crc32(&payload).to_le_bytes());
    out
}

pub(crate) struct Loaded {
    pub bytes: Vec<u8>,
    pub wal_len: u64,
}

pub(crate) fn load(path: &Path) -> Result<Loaded> {
    let mut bytes = fs::read(path)?;
    storage::decode(&bytes)?;
    let (identity, mut sequence) = storage::header(&bytes);
    let log = match fs::read(sidecar(path, ".wal")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded { bytes, wal_len: 0 });
        }
        Err(e) => return Err(e.into()),
    };
    if log.is_empty() {
        return Ok(Loaded { bytes, wal_len: 0 });
    }
    if log.len() < HEADER && header(identity).starts_with(&log) {
        return Ok(Loaded { bytes, wal_len: 0 });
    }
    if log.len() < HEADER || &log[..8] != MAGIC {
        return Err(corrupt("invalid WAL header"));
    }
    if log[8..24] != identity.to_le_bytes() {
        return Err(corrupt("WAL belongs to another database"));
    }
    let mut pos = HEADER;
    let mut previous = None;
    while log.len() - pos >= 12 {
        if storage::crc32(&log[pos..pos + 8])
            != u32::from_le_bytes(log[pos + 8..pos + 12].try_into().unwrap())
        {
            return Err(corrupt("WAL length checksum mismatch"));
        }
        let len = u64::from_le_bytes(log[pos..pos + 8].try_into().unwrap());
        let Some(end) = usize::try_from(len)
            .ok()
            .and_then(|n| pos.checked_add(12)?.checked_add(n)?.checked_add(4))
        else {
            return Err(corrupt("WAL frame length overflow"));
        };
        if end > log.len() {
            break;
        } // Crash before the commit checksum was written.
        let payload = &log[pos + 12..end - 4];
        if storage::crc32(payload) != u32::from_le_bytes(log[end - 4..end].try_into().unwrap()) {
            return Err(corrupt("WAL checksum mismatch"));
        }
        if payload.len() < 20 {
            return Err(corrupt("short WAL frame"));
        }
        let seq = u64::from_le_bytes(payload[..8].try_into().unwrap());
        if previous.is_some_and(|n: u64| n.checked_add(1) != Some(seq)) {
            return Err(corrupt("WAL sequence gap"));
        }
        previous = Some(seq);
        if seq > sequence {
            if sequence.checked_add(1) != Some(seq) {
                return Err(corrupt("WAL sequence gap"));
            }
            if bytes[bytes.len() - 4..] != payload[16..20] {
                return Err(corrupt("WAL base checksum mismatch"));
            }
            let len = usize::try_from(u64::from_le_bytes(payload[8..16].try_into().unwrap()))
                .map_err(|_| corrupt("snapshot too large"))?;
            // The payload must supply all growth, so a corrupt length cannot allocate unbounded memory.
            if len > bytes.len().saturating_add(payload.len()) {
                return Err(corrupt("invalid WAL image length"));
            }
            bytes.resize(len, 0);
            let mut reader = &payload[20..];
            let mut previous_end = 0;
            while !reader.is_empty() {
                let mut address = [0; 8];
                reader
                    .read_exact(&mut address)
                    .map_err(|_| corrupt("short WAL page"))?;
                let start = usize::try_from(u64::from_le_bytes(address))
                    .map_err(|_| corrupt("WAL offset overflow"))?;
                let mut size = [0; 4];
                reader
                    .read_exact(&mut size)
                    .map_err(|_| corrupt("short WAL page"))?;
                let size = u32::from_le_bytes(size) as usize;
                let end = start
                    .checked_add(size)
                    .filter(|&e| e <= bytes.len())
                    .ok_or_else(|| corrupt("WAL page out of range"))?;
                if start < previous_end || start % PAGE != 0 || size == 0 || size > PAGE {
                    return Err(corrupt("invalid WAL page"));
                }
                reader
                    .read_exact(&mut bytes[start..end])
                    .map_err(|_| corrupt("short WAL page"))?;
                previous_end = end;
            }
            if storage::header(&bytes) != (identity, seq) {
                return Err(corrupt("WAL image header mismatch"));
            }
            sequence = seq;
        }
        pos = end;
    }
    storage::decode(&bytes)?;
    Ok(Loaded {
        bytes,
        wal_len: pos as u64,
    })
}

pub(crate) fn append(path: &Path, valid_len: u64, identity: u128, frame: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sidecar(path, ".wal"))?;
    file.set_len(valid_len)?; // Discard an uncommitted tail before retrying/appending.
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::End(0))?;
    if valid_len == 0 {
        file.write_all(&header(identity))?;
    }
    file.write_all(frame)?;
    file.sync_all()?;
    super::database::sync_parent_dir(path);
    Ok(())
}
fn corrupt(message: &str) -> Error {
    Error::Corrupt(message.into())
}
