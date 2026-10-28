use super::*;

// ---------------------------------------------------------------- listing

pub fn list(path: &Path, password: Option<&str>) -> Result<Vec<Entry>> {
    let fmt = detect_or_err(path)?;
    let mut out = Vec::new();
    match fmt {
        Format::Zip => {
            let mut z = zip::ZipArchive::new(BufReader::new(File::open(path)?))?;
            for i in 0..z.len() {
                let f = z.by_index_raw(i)?;
                let modified = f.last_modified().map(|d| {
                    format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year(), d.month(), d.day(), d.hour(), d.minute())
                });
                out.push(Entry {
                    name: f.name()?.to_string(),
                    size: f.size(),
                    compressed: Some(f.compressed_size()),
                    modified,
                    is_dir: f.is_dir(),
                    crc32: if f.is_dir() { None } else { Some(f.crc32()) },
                    encrypted: f.encrypted(),
                });
            }
        }
        Format::SevenZ => {
            let pw = password.map(sevenz_rust2::Password::from).unwrap_or_else(sevenz_rust2::Password::empty);
            let r = sevenz_rust2::ArchiveReader::open(path, pw).map_err(|e| Error::SevenZ(e.to_string()))?;
            let a = r.archive();
            let aes = |b: usize| a.blocks[b].coders.iter().any(|c| c.encoder_method_id() == sevenz_rust2::EncoderMethod::ID_AES256_SHA256);
            for (i, e) in a.files.iter().enumerate() {
                let modified = if e.has_last_modified_date {
                    let st: std::time::SystemTime = e.last_modified_date.into();
                    st.duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| fmt_time(d.as_secs() as i64))
                } else { None };
                out.push(Entry {
                    name: e.name.clone(),
                    size: e.size,
                    compressed: if e.compressed_size > 0 { Some(e.compressed_size) } else { None },
                    modified,
                    is_dir: e.is_directory,
                    crc32: if e.has_crc { Some(e.crc as u32) } else { None },
                    encrypted: a.stream_map.file_block_index[i].is_some_and(aes),
                });
            }
        }
        Format::Rar => {
            let a = match password {
                Some(p) => unrar::Archive::with_password(path, p),
                None => unrar::Archive::new(path),
            };
            let it = a.open_for_listing().map_err(|e| Error::Rar(e.to_string()))?;
            for h in it {
                let h = h.map_err(|e| Error::Rar(e.to_string()))?;
                out.push(Entry {
                    name: h.filename.to_string_lossy().replace('\\', "/"),
                    size: h.unpacked_size,
                    compressed: None,
                    modified: None,
                    is_dir: h.is_directory(),
                    crc32: if h.is_directory() { None } else { Some(h.file_crc) },
                    encrypted: h.is_encrypted(),
                });
            }
        }
        f if f.is_single_file() => {
            let compressed = fs::metadata(path)?.len();
            let mut d = decoder(f, rd(File::open(path)?))?;
            let size = io::copy(&mut d, &mut io::sink())?;
            out.push(Entry { name: single_inner_name(path), size, compressed: Some(compressed), modified: None, is_dir: false, crc32: None, encrypted: false });
        }
        f => {
            let mut a = tar::Archive::new(decoder(f, rd(File::open(path)?))?);
            for e in a.entries()? {
                let e = e?;
                let h = e.header();
                out.push(Entry {
                    name: e.path()?.to_string_lossy().to_string(),
                    size: h.size().unwrap_or(0),
                    compressed: None,
                    modified: h.mtime().ok().and_then(|t| fmt_time(t as i64)),
                    is_dir: h.entry_type().is_dir(),
                    crc32: None,
                    encrypted: false,
                });
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- extraction

/// Extract `archive` into `dest`. `selected` limits extraction to the given entry names (or folders).
pub fn extract(archive: &Path, dest: &Path, password: Option<&str>, selected: Option<&[String]>, p: &Progress) -> Result<()> {
    let fmt = detect_or_err(archive)?;
    fs::create_dir_all(dest)?;
    let done = AtomicU64::new(0);
    match fmt {
        Format::Zip => extract_zip(archive, dest, password, selected, threads() as usize, p)?,
        Format::SevenZ => {
            let pw = password.map(sevenz_rust2::Password::from).unwrap_or_else(sevenz_rust2::Password::empty);
            let mut r = sevenz_rust2::ArchiveReader::open(archive, pw).map_err(|e| Error::SevenZ(e.to_string()))?;
            r.set_thread_count(threads());
            let total: u64 = r.archive().files.iter().filter(|e| wanted(selected, &e.name)).map(|e| e.size).sum();
            let mut err: Option<Error> = None;
            r.for_each_entries(|e, rd| {
                let skip = |rd: &mut dyn Read| io::copy(rd, &mut io::sink()).map(|_| true).map_err(Into::into);
                if !wanted(selected, &e.name) { return skip(rd); }
                let Some(out) = safe_join(dest, &e.name) else { return skip(rd) };
                let res = (|| -> Result<()> {
                    p.check()?;
                    if e.is_directory {
                        fs::create_dir_all(&out)?;
                    } else {
                        if let Some(par) = out.parent() { fs::create_dir_all(par)?; }
                        let mut w = wr(File::create(&out)?);
                        copy_prog(rd, &mut w, &done, total, &e.name, p)?;
                        let f = w.into_inner().map_err(|e| e.into_error())?;
                        if e.has_last_modified_date { let _ = f.set_modified(e.last_modified_date.into()); }
                    }
                    Ok(())
                })();
                match res {
                    Ok(()) => Ok(true),
                    Err(x) => { err = Some(x); Ok(false) }
                }
            }).map_err(|e| Error::SevenZ(e.to_string()))?;
            if let Some(e) = err { return Err(e); }
        }
        Format::Rar => {
            let entries = list(archive, password)?;
            let total: u64 = entries.iter().filter(|e| wanted(selected, &e.name)).map(|e| e.size).sum();
            let a = match password {
                Some(pw) => unrar::Archive::with_password(archive, pw),
                None => unrar::Archive::new(archive),
            };
            let mut cur = a.open_for_processing().map_err(|e| Error::Rar(e.to_string()))?;
            while let Some(h) = cur.read_header().map_err(|e| Error::Rar(e.to_string()))? {
                p.check()?;
                let name = h.entry().filename.to_string_lossy().replace('\\', "/");
                let size = h.entry().unpacked_size;
                cur = if wanted(selected, &name) && safe_join(dest, &name).is_some() {
                    let c = h.extract_with_base(dest).map_err(|e| Error::Rar(e.to_string()))?;
                    let d = done.fetch_add(size, Ordering::Relaxed) + size;
                    (p.on)(d, total, &name);
                    c
                } else {
                    h.skip().map_err(|e| Error::Rar(e.to_string()))?
                };
            }
        }
        f if f.is_single_file() => {
            let total = fs::metadata(archive)?.len();
            let name = single_inner_name(archive);
            let d = decoder(f, rd(File::open(archive)?))?;
            let mut w = wr(File::create(dest.join(&name))?);
            // progress is approximate (decompressed bytes vs compressed size)
            copy_prog(d, &mut w, &done, total.max(1), &name, p)?;
        }
        f => {
            let total = fs::metadata(archive)?.len();
            let file = File::open(archive)?;
            let counted = CountingReader { inner: BufReader::new(file), count: Arc::new(std::sync::atomic::AtomicU64::new(0)) };
            let counter = counted.count.clone();
            let mut a = tar::Archive::new(decoder(f, counted)?);
            for e in a.entries()? {
                p.check()?;
                let mut e = e?;
                let name = e.path()?.to_string_lossy().to_string();
                if wanted(selected, &name) {
                    e.unpack_in(dest)?;
                }
                (p.on)(counter.load(Ordering::Relaxed), total, &name);
            }
        }
    }
    Ok(())
}

pub(super) fn open_zip(archive: &Path) -> Result<zip::ZipArchive<BufReader<File>>> {
    Ok(zip::ZipArchive::new(BufReader::with_capacity(64 << 10, File::open(archive)?))?)
}

/// ZIP extraction. Entries are chosen from the central directory alone, so unselected entries are
/// never opened or decrypted. Large jobs are split into size-balanced groups (longest first),
/// each extracted by its own thread with its own file handle.
pub(super) fn extract_zip(archive: &Path, dest: &Path, password: Option<&str>, selected: Option<&[String]>, threads: usize, p: &Progress) -> Result<()> {
    let mut z = open_zip(archive)?;
    let (mut files, mut total) = (Vec::new(), 0);
    // entries landing on the same file (case-insensitive file systems, "a/../b" vs "b") must not be
    // written by two workers at once: such archives are extracted sequentially (last one wins)
    let (mut seen, mut collide) = (std::collections::HashSet::new(), false);
    for i in 0..z.len() {
        let f = z.by_index_data(i)?;
        if !wanted(selected, &f.name()?) { continue; }
        let Some(rel) = f.enclosed_name() else { continue };
        if f.is_dir() {
            fs::create_dir_all(dest.join(rel))?;
        } else {
            collide |= !seen.insert(rel.to_string_lossy().to_lowercase());
            total += f.size();
            files.push((i, f.size()));
        }
    }
    let done = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let t = threads.min(files.len());
    if t < 2 || total < ZIP_PARALLEL_MIN || collide {
        let idx: Vec<usize> = files.iter().map(|f| f.0).collect();
        return zip_extract_files(&mut z, dest, password, &idx, &done, total, p, &stop);
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    let mut bins: Vec<(u64, Vec<usize>)> = (0..t).map(|_| (0, Vec::new())).collect();
    for (i, sz) in files {
        let b = bins.iter_mut().min_by_key(|b| b.0).unwrap();
        b.0 += sz;
        b.1.push(i);
    }
    let mut first = Some(z);
    std::thread::scope(|sc| {
        let hs: Vec<_> = bins.iter_mut().map(|(_, idx)| {
            idx.sort_unstable(); // archive order: sequential reads within each worker
            let (z, done, stop) = (first.take(), &done, &stop);
            sc.spawn(move || -> Result<()> {
                let r = match z { Some(z) => Ok(z), None => open_zip(archive) }
                    .and_then(|mut z| zip_extract_files(&mut z, dest, password, idx, done, total, p, stop));
                if r.is_err() { stop.store(true, Ordering::Relaxed); }
                r
            })
        }).collect();
        let mut first_err = None;
        for h in hs {
            if let Err(e) = h.join().unwrap_or_else(|_| Err(Error::Io(io::Error::other("worker panicked")))) && first_err.is_none() {
                first_err = Some(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn zip_extract_files<R: Read + io::Seek>(z: &mut zip::ZipArchive<R>, dest: &Path, password: Option<&str>, idx: &[usize], done: &AtomicU64, total: u64, p: &Progress, stop: &AtomicBool) -> Result<()> {
    for &i in idx {
        p.check()?;
        if stop.load(Ordering::Relaxed) { break; }
        let mut f = match password {
            Some(pw) => z.by_index_decrypt(i, pw.as_bytes())?,
            None => z.by_index(i)?,
        };
        let name = f.name()?.to_string();
        let Some(out) = f.enclosed_name().map(|rel| dest.join(rel)) else { continue };
        if let Some(par) = out.parent() { fs::create_dir_all(par)?; }
        let mut w = BufWriter::with_capacity((f.size() as usize).clamp(8 << 10, BUF), File::create(&out)?);
        copy_prog(&mut f, &mut w, done, total, &name, p)?;
        let file = w.into_inner().map_err(|e| e.into_error())?;
        if let Some(t) = f.last_modified().and_then(zip_mtime) { let _ = file.set_modified(t); }
    }
    Ok(())
}

/// ZIP (MS-DOS) timestamps are local time.
pub(super) fn zip_mtime(d: zip::DateTime) -> Option<std::time::SystemTime> {
    use chrono::TimeZone;
    let t = chrono::NaiveDate::from_ymd_opt(d.year().into(), d.month().into(), d.day().into())?.and_hms_opt(d.hour().into(), d.minute().into(), d.second().into())?;
    chrono::Local.from_local_datetime(&t).earliest().map(Into::into)
}

/// Test archive integrity by decompressing every entry (CRCs / checksums are verified by
/// the decoders). Returns the number of files tested.
pub fn test(archive: &Path, password: Option<&str>, p: &Progress) -> Result<usize> {
    let fmt = detect_or_err(archive)?;
    let done = AtomicU64::new(0);
    let mut n = 0usize;
    match fmt {
        Format::Zip => {
            let mut z = zip::ZipArchive::new(BufReader::with_capacity(64 << 10, File::open(archive)?))?;
            let total: u64 = (0..z.len()).filter_map(|i| z.by_index_raw(i).ok().map(|f| f.size())).sum();
            for i in 0..z.len() {
                p.check()?;
                let mut f = match password {
                    Some(pw) => z.by_index_decrypt(i, pw.as_bytes())?,
                    None => z.by_index(i)?,
                };
                if f.is_dir() { continue; }
                let name = f.name()?.to_string();
                copy_prog(&mut f, &mut io::sink(), &done, total, &name, p)?;
                n += 1;
            }
        }
        Format::SevenZ => {
            let pw = password.map(sevenz_rust2::Password::from).unwrap_or_else(sevenz_rust2::Password::empty);
            let mut r = sevenz_rust2::ArchiveReader::open(archive, pw).map_err(|e| Error::SevenZ(e.to_string()))?;
            r.set_thread_count(threads());
            let total: u64 = r.archive().files.iter().map(|e| e.size).sum();
            let mut err = None;
            r.for_each_entries(|e, rd| {
                if e.is_directory { return Ok(true); }
                match copy_prog(rd, &mut io::sink(), &done, total, &e.name, p) {
                    Ok(()) => { n += 1; Ok(true) }
                    Err(x) => { err = Some(x); Ok(false) }
                }
            }).map_err(|e| Error::SevenZ(e.to_string()))?;
            if let Some(e) = err { return Err(e); }
        }
        Format::Rar => {
            let a = match password {
                Some(pw) => unrar::Archive::with_password(archive, pw),
                None => unrar::Archive::new(archive),
            };
            let mut cur = a.open_for_processing().map_err(|e| Error::Rar(e.to_string()))?;
            while let Some(h) = cur.read_header().map_err(|e| Error::Rar(e.to_string()))? {
                p.check()?;
                let file = h.entry().is_file();
                cur = h.test().map_err(|e| Error::Rar(e.to_string()))?;
                if file { n += 1; }
            }
        }
        f if f.is_single_file() => {
            let total = fs::metadata(archive)?.len();
            let d = decoder(f, rd(File::open(archive)?))?;
            copy_prog(d, &mut io::sink(), &done, total, &single_inner_name(archive), p)?;
            n = 1;
        }
        f => {
            let mut a = tar::Archive::new(decoder(f, rd(File::open(archive)?))?);
            for e in a.entries()? {
                p.check()?;
                let mut e = e?;
                if e.header().entry_type().is_file() {
                    io::copy(&mut e, &mut io::sink())?;
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}

/// Delete entries (files, or folders including their contents) from an archive by rewriting it.
/// ZIP entries are copied raw (no recompression); other formats are re-encoded.
pub fn delete(archive: &Path, names: &[String], password: Option<&str>, p: &Progress) -> Result<()> {
    let fmt = detect_or_err(archive)?;
    if !fmt.can_create() || fmt.is_single_file() {
        return Err(Error::Unsupported(format!("cannot delete entries from {}", fmt.label())));
    }
    let tmp = {
        let mut n = archive.as_os_str().to_owned();
        n.push(".rewrite.tmp");
        PathBuf::from(n)
    };
    let hit = |name: &str| wanted(Some(names), name);
    let res = (|| -> Result<()> {
        if fmt == Format::Zip {
            let mut src = zip::ZipArchive::new(BufReader::with_capacity(256 << 10, File::open(archive)?))?;
            let mut out = zip::ZipWriter::new(wr(File::create(&tmp)?));
            for i in 0..src.len() {
                p.check()?;
                let f = src.by_index_raw(i)?;
                if !hit(&f.name()?) {
                    out.raw_copy_file(f)?;
                }
            }
            out.finish()?.flush()?;
            return Ok(());
        }
        // generic: extract kept entries to a temp dir, then recreate with the same format
        let work = tempdir_near(archive)?;
        let res = (|| -> Result<()> {
            let kept: Vec<Entry> = list(archive, password)?.into_iter().filter(|e| !hit(&e.name)).collect();
            // select files only (a folder name would select its deleted children too)
            let keep: Vec<String> = kept.iter().filter(|e| !e.is_dir).map(|e| e.name.clone()).collect();
            if !keep.is_empty() {
                extract(archive, &work, password, Some(&keep), p)?;
            }
            for d in kept.iter().filter(|e| e.is_dir) {
                if let Some(path) = safe_join(&work, &d.name) {
                    fs::create_dir_all(path)?;
                }
            }
            let mut inputs: Vec<PathBuf> = fs::read_dir(&work)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
            inputs.sort();
            if inputs.is_empty() {
                return Err(Error::Unsupported("cannot delete every entry of an archive".into()));
            }
            // tmp must carry the archive's extension so `create` picks the right format
            let opts = CreateOptions { format: fmt, level: None, password: password.map(str::to_string) };
            create(&tmp, &inputs, &opts, p)
        })();
        let _ = fs::remove_dir_all(&work);
        res
    })();
    match res {
        Ok(()) => {
            fs::rename(&tmp, archive)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub(super) fn tempdir_near(archive: &Path) -> Result<PathBuf> {
    let base = std::env::temp_dir();
    let stem = archive.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    for i in 0..1000u32 {
        let d = base.join(format!("archiver-{}-{}-{i}", std::process::id(), stem));
        if fs::create_dir(&d).is_ok() {
            return Ok(d);
        }
    }
    Err(Error::Io(io::Error::other("cannot create temp dir")))
}

struct CountingReader<R> { inner: R, count: Arc<std::sync::atomic::AtomicU64> }
impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(b)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}
