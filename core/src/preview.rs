//! Read a single archive entry into memory (preview pane, quick copy) without extracting the
//! archive to disk.

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Error, Format, ProgReader, Progress, Result, copy_prog, decoder, detect_or_err, norm_name, rd, single_inner_name, tempdir_near, threads};

/// Data of one entry, possibly cut off at the requested limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryData {
    pub bytes: Vec<u8>,
    /// The entry is longer than the limit; `bytes` holds only its first `max` bytes.
    pub truncated: bool,
}

/// Read up to `max` bytes of the file entry `name` (as listed by [`crate::list`]).
/// Fails with [`crate::Error::NotFound`] if there is no such file entry.
pub fn read_entry(archive: &Path, name: &str, password: Option<&str>, max: usize, p: &Progress) -> Result<EntryData> {
    let fmt = detect_or_err(archive)?;
    let key = norm_name(name);
    let name = key.as_str();
    let nf = || Error::NotFound(name.to_string());
    let same = |n: &str| norm_name(n) == name;
    p.check()?;
    match fmt {
        Format::Zip => {
            let mut z = zip::ZipArchive::new(BufReader::with_capacity(64 << 10, File::open(archive)?))?;
            let is_file = |z: &zip::ZipArchive<_>, i| z.by_index_data(i).is_ok_and(|f| !f.is_dir() && f.name().is_ok_and(|n| same(&n)));
            // fast path: lookup by raw name; else scan (non-UTF-8 names, "dir" given for "dir/")
            let idx = z.index_for_name(name).filter(|&i| is_file(&z, i)).or_else(|| (0..z.len()).find(|&i| is_file(&z, i))).ok_or_else(nf)?;
            let f = match password {
                Some(pw) => z.by_index_decrypt(idx, pw.as_bytes())?,
                None => z.by_index(idx)?,
            };
            let size = f.size();
            read_limited(f, size, name, max, p)
        }
        Format::SevenZ => {
            let pw = password.map(sevenz_rust2::Password::from).unwrap_or_else(sevenz_rust2::Password::empty);
            let mut src = BufReader::with_capacity(64 << 10, File::open(archive)?);
            let a = sevenz_rust2::Archive::read(&mut src, &pw).map_err(|e| Error::SevenZ(e.to_string()))?;
            let i = a.files.iter().position(|e| !e.is_directory && same(&e.name)).ok_or_else(nf)?;
            let Some(block) = a.stream_map.file_block_index[i] else {
                return Ok(EntryData { bytes: Vec::new(), truncated: false });
            };
            let target = &a.files[i];
            // only the entry's block is decoded; entries before it in a solid block are skip-read
            let first = a.stream_map.block_first_file_index[block];
            let before: u64 = a.files[first.min(i)..i].iter().map(|e| e.size).sum();
            let skipped = AtomicU64::new(0);
            let mut out: Option<Result<EntryData>> = None;
            sevenz_rust2::BlockDecoder::new(threads(), block, &a, &pw, &mut src)
                .for_each_entries(&mut |e, r| {
                    if std::ptr::eq(e, target) {
                        out = Some(read_limited(r, e.size, name, max, p));
                        return Ok(false);
                    }
                    match copy_prog(r, &mut io::sink(), &skipped, before, &e.name, p) {
                        Ok(()) => Ok(true),
                        Err(x) => { out = Some(Err(x)); Ok(false) }
                    }
                })
                .map_err(|e| if p.cancel.load(Ordering::Relaxed) { Error::Cancelled } else { Error::SevenZ(e.to_string()) })?;
            out.unwrap_or_else(|| Err(nf()))
        }
        Format::Rar => {
            let rar = |e: unrar::error::UnrarError| Error::Rar(e.to_string());
            let a = match password {
                Some(pw) => unrar::Archive::with_password(archive, pw),
                None => unrar::Archive::new(archive),
            };
            let mut cur = a.open_for_processing().map_err(rar)?;
            while let Some(h) = cur.read_header().map_err(rar)? {
                p.check()?;
                let e = h.entry();
                if e.is_directory() || !same(&e.filename.to_string_lossy().replace('\\', "/")) {
                    cur = h.skip().map_err(rar)?;
                    continue;
                }
                let size = e.unpacked_size;
                // the header size is trusted for the in-memory read only below a sane bound
                if size <= max as u64 && size <= RAR_PREVIEW_MAX {
                    let (mut bytes, _) = h.read().map_err(rar)?;
                    let truncated = bytes.len() > max;
                    bytes.truncate(max);
                    return Ok(EntryData { bytes, truncated });
                }
                // unrar cannot stop mid-entry: refuse to unpack huge entries just for a preview
                if size > RAR_PREVIEW_MAX {
                    return Err(Error::Unsupported(format!("{name}: RAR entry too large to preview")));
                }
                // too big to hold in memory: unpack into a temp dir and read the head of the file
                let tmp = tempdir_near(archive)?;
                let file = tmp.join("entry");
                let res = h.extract_to(&file).map_err(rar).and_then(|_| read_limited(File::open(&file)?, size, name, max, p));
                let _ = fs::remove_dir_all(&tmp);
                return res;
            }
            Err(nf())
        }
        f if f.is_single_file() => {
            if name != single_inner_name(archive) {
                return Err(nf());
            }
            let hint = fs::metadata(archive)?.len();
            read_limited(decoder(f, rd(File::open(archive)?))?, hint, name, max, p)
        }
        f => {
            let mut a = tar::Archive::new(decoder(f, rd(File::open(archive)?))?);
            for e in a.entries()? {
                p.check()?;
                let e = e?;
                if !e.header().entry_type().is_dir() && same(&e.path()?.to_string_lossy()) {
                    let size = e.header().size().unwrap_or(0);
                    return read_limited(e, size, name, max, p);
                }
            }
            Err(nf())
        }
    }
}

/// Largest RAR entry unpacked (in full, uncancellable) for a preview.
const RAR_PREVIEW_MAX: u64 = 64 << 20;

/// Read at most `max` bytes (one more is read to detect truncation); `hint` is the expected size.
fn read_limited(r: impl Read, hint: u64, name: &str, max: usize, p: &Progress) -> Result<EntryData> {
    let done = AtomicU64::new(0);
    let want = hint.min(max as u64);
    let mut bytes = Vec::with_capacity(want.min(64 << 20) as usize);
    let res = ProgReader::new(r, &done, want, name, p).take((max as u64).saturating_add(1)).read_to_end(&mut bytes);
    match res {
        Ok(_) => {}
        Err(_) if p.cancel.load(Ordering::Relaxed) => return Err(Error::Cancelled),
        Err(e) => return Err(e.into()),
    }
    let truncated = bytes.len() > max;
    bytes.truncate(max);
    Ok(EntryData { bytes, truncated })
}
