//! Checksummed, committed page after-images. An incomplete final frame is ignored.
use crate::{
    error::{Error, Result},
    storage,
};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
const MAGIC: &[u8; 8] = b"RVWAL\0\x01\0";
const PAGE: usize = 4096;
pub(crate) const HEADER: usize = 24;

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
    pub commits: u64,
    pub base: Stamp,
}

pub(crate) fn load(path: &Path) -> Result<Loaded> {
    let mut bytes = fs::read(path)?;
    storage::decode(&bytes)?;
    let base = Stamp::from_bytes(&bytes);
    let (identity, mut sequence) = storage::header(&bytes);
    let log = match fs::read(sidecar(path, ".wal")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                bytes,
                wal_len: 0,
                commits: 0,
                base,
            });
        }
        Err(e) => return Err(e.into()),
    };
    if log.is_empty() {
        return Ok(Loaded {
            bytes,
            wal_len: 0,
            commits: 0,
            base,
        });
    }
    if log.len() < HEADER && header(identity).starts_with(&log) {
        return Ok(Loaded {
            bytes,
            wal_len: 0,
            commits: 0,
            base,
        });
    }
    if log.len() < HEADER || &log[..8] != MAGIC {
        return Err(corrupt("invalid WAL header"));
    }
    if log[8..24] != identity.to_le_bytes() {
        return Err(corrupt("WAL belongs to another database"));
    }
    let mut pos = HEADER;
    let mut previous = None;
    let mut commits = 0;
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
            commits += 1;
        }
        pos = end;
    }
    storage::decode(&bytes)?;
    Ok(Loaded {
        bytes,
        wal_len: pos as u64,
        commits,
        base,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub version: u32,
    pub id: u128,
    pub sequence: u64,
    pub checksum: u32,
    pub len: u64,
}
impl Stamp {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let (id, sequence) = storage::header(bytes);
        Self {
            version: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            id,
            sequence,
            checksum: u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap()),
            len: bytes.len() as u64,
        }
    }
}
pub(crate) struct Current {
    pub base: Stamp,
    pub wal_len: u64,
    pub commits: u64,
    pub read_bytes: u64,
}
fn stale() -> Error {
    Error::InvalidArgument("database changed in another handle; reopen before writing".into())
}
/// Validate only the immutable snapshot header/footer and any WAL bytes after
/// this handle's validated prefix. Callers hold the exclusive database lock.
/// Arbitrary external file rewriting is unsupported, as with the legacy path.
pub(crate) fn current(
    path: &Path,
    cached_base: Stamp,
    current: Stamp,
    valid_len: u64,
    commits: u64,
) -> Result<Current> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len < 4100 || (len - 4) % 4096 != 0 {
        return Err(corrupt("invalid paged snapshot size"));
    }
    let mut head = [0u8; 4096];
    file.read_exact(&mut head)?;
    if &head[..4] != b"RVEC"
        || u32::from_le_bytes(head[4..8].try_into().unwrap()) != 3
        || storage::crc32(&head[..4092]) != u32::from_le_bytes(head[4092..].try_into().unwrap())
        || u64::from_le_bytes(head[32..40].try_into().unwrap()) != (len - 4) / 4096
    {
        return Err(corrupt("invalid paged snapshot header"));
    }
    file.seek(SeekFrom::End(-4))?;
    let mut crc = [0; 4];
    file.read_exact(&mut crc)?;
    let base = Stamp {
        version: 3,
        id: u128::from_le_bytes(head[8..24].try_into().unwrap()),
        sequence: u64::from_le_bytes(head[24..32].try_into().unwrap()),
        checksum: u32::from_le_bytes(crc),
        len,
    };
    if base != cached_base && base != current {
        return Err(stale());
    }
    let mut result = Current {
        base,
        wal_len: 0,
        commits: if base == current { 0 } else { commits },
        read_bytes: 4100,
    };
    let mut log = match fs::File::open(sidecar(path, ".wal")) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if base != current {
                return Err(corrupt("committed WAL is missing"));
            }
            return Ok(result);
        }
        Err(e) => return Err(e.into()),
    };
    let size = log.metadata()?.len();
    let mut h = vec![0; size.min(HEADER as u64) as usize];
    log.read_exact(&mut h)?;
    result.read_bytes += h.len() as u64;
    if h.len() < HEADER {
        if !header(current.id).starts_with(&h) || base != current {
            return Err(corrupt("truncated committed WAL"));
        }
        return Ok(result);
    }
    if h != header(current.id) {
        return Err(corrupt("WAL belongs to another database"));
    }
    let start = if base == cached_base && base.sequence < current.sequence {
        if size < valid_len {
            return Err(corrupt("committed WAL was truncated"));
        }
        valid_len.max(HEADER as u64)
    } else {
        HEADER as u64
    };
    log.seek(SeekFrom::Start(start))?;
    let mut pos = start;
    let mut previous = None;
    while size - pos >= 12 {
        let mut prefix = [0; 12];
        log.read_exact(&mut prefix)?;
        result.read_bytes += 12;
        if storage::crc32(&prefix[..8]) != u32::from_le_bytes(prefix[8..].try_into().unwrap()) {
            return Err(corrupt("WAL length checksum mismatch"));
        }
        let n = u64::from_le_bytes(prefix[..8].try_into().unwrap());
        let end = pos
            .checked_add(16)
            .and_then(|p| p.checked_add(n))
            .ok_or_else(|| corrupt("WAL length overflow"))?;
        if end > size {
            break;
        }
        if n < 20 {
            return Err(corrupt("short WAL frame"));
        }
        let mut first = [0; 20];
        log.read_exact(&mut first)?;
        result.read_bytes += 20;
        let seq = u64::from_le_bytes(first[..8].try_into().unwrap());
        if seq > current.sequence {
            return Err(stale());
        }
        if previous.is_some_and(|p: u64| p.checked_add(1) != Some(seq)) {
            return Err(corrupt("WAL sequence gap"));
        }
        previous = Some(seq);
        if start > HEADER as u64 {
            return Err(corrupt("unexpected WAL tail sequence"));
        }
        let mut crc = storage::Crc32::new();
        crc.update(&first);
        let mut left = n - 20;
        let mut buffer = [0u8; 8192];
        while left > 0 {
            let size = left.min(buffer.len() as u64) as usize;
            log.read_exact(&mut buffer[..size])?;
            crc.update(&buffer[..size]);
            left -= size as u64;
            result.read_bytes += size as u64;
        }
        let mut tail = [0; 4];
        log.read_exact(&mut tail)?;
        result.read_bytes += 4;
        if crc.finish() != u32::from_le_bytes(tail) {
            return Err(corrupt("WAL checksum mismatch"));
        }
        pos = end;
    }
    result.wal_len = pos;
    Ok(result)
}

pub(crate) fn frame_pages(
    previous: u32,
    len: u64,
    sequence: u64,
    pages: impl Iterator<Item = (u64, Vec<u8>)>,
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend(sequence.to_le_bytes());
    payload.extend(len.to_le_bytes());
    payload.extend(previous.to_le_bytes());
    for (offset, bytes) in pages {
        payload.extend(offset.to_le_bytes());
        payload.extend((bytes.len() as u32).to_le_bytes());
        payload.extend(bytes);
    }
    let mut out = (payload.len() as u64).to_le_bytes().to_vec();
    out.extend(storage::crc32(&out).to_le_bytes());
    out.extend(&payload);
    out.extend(storage::crc32(&payload).to_le_bytes());
    out
}

pub(crate) fn append(path: &Path, valid_len: u64, identity: u128, frame: &[u8]) -> Result<()> {
    append_with(path, valid_len, identity, frame, || Ok(()))
}
pub(crate) fn append_with(
    path: &Path,
    valid_len: u64,
    identity: u128,
    frame: &[u8],
    written: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(sidecar(path, ".wal"))?;
    file.set_len(valid_len)?; // Discard an uncommitted tail before retrying/appending.
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::End(0))?;
    if valid_len == 0 {
        file.write_all(&header(identity))?;
    }
    file.write_all(frame)?;
    written()?;
    file.sync_all()?;
    super::database::sync_parent_dir(path)?;
    Ok(())
}
fn corrupt(message: &str) -> Error {
    Error::Corrupt(message.into())
}
