use super::*;

// ---------------------------------------------------------------- creation

#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub format: Format,
    /// Compression level (see Format::level_range); None = default.
    pub level: Option<u32>,
    pub password: Option<String>,
}

/// Expand inputs into (absolute path, archive name, is_dir).
pub(super) fn collect(inputs: &[PathBuf]) -> Result<Vec<(PathBuf, String, bool)>> {
    let mut v = Vec::new();
    for inp in inputs {
        let base = inp.parent().unwrap_or(Path::new(""));
        for e in WalkDir::new(inp).sort_by_file_name() {
            let e = e.map_err(|e| io::Error::other(e.to_string()))?;
            let rel = e.path().strip_prefix(base).unwrap_or(e.path());
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            if name.is_empty() { continue; }
            v.push((e.path().to_path_buf(), name, e.file_type().is_dir()));
        }
    }
    Ok(v)
}

pub(super) fn zip_options<'a>(path: &Path, opts: &'a CreateOptions) -> zip::write::FileOptions<'a, 'static, ()> {
    let lvl = opts.level.unwrap_or(6);
    let mut o = zip::write::FileOptions::<'a, 'static, ()>::default()
        .compression_method(if lvl == 0 { zip::CompressionMethod::Stored } else { zip::CompressionMethod::Deflated })
        .compression_level(if lvl == 0 { None } else { Some(lvl as i64) })
        .large_file(true);
    if let Some(pw) = opts.password.as_deref().filter(|s| !s.is_empty()) {
        o = o.with_aes_encryption(zip::AesMode::Aes256, pw);
    }
    if let Ok(m) = fs::metadata(path).and_then(|m| m.modified()) {
        let dt: chrono::DateTime<chrono::Local> = m.into();
        use chrono::{Datelike, Timelike};
        if let Ok(d) = zip::DateTime::from_date_and_time(dt.year() as u16, dt.month() as u8, dt.day() as u8, dt.hour() as u8, dt.minute() as u8, dt.second() as u8) {
            o = o.last_modified_time(d);
        }
    }
    o
}

/// Per-file reader sized to the file (small files don't get a 1 MiB buffer).
pub(super) fn rd_file(path: &Path) -> Result<BufReader<File>> {
    let f = File::open(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0) as usize;
    Ok(BufReader::with_capacity(len.clamp(8 << 10, BUF), f))
}

type Item = (PathBuf, String, bool);

pub(super) fn zip_write_items<W: Write + io::Seek>(z: &mut zip::ZipWriter<W>, items: &[&Item], opts: &CreateOptions, done: &AtomicU64, total: u64, p: &Progress) -> Result<()> {
    for (path, name, is_dir) in items.iter().map(|i| (&i.0, &i.1, i.2)) {
        p.check()?;
        let o = zip_options(path, opts);
        if is_dir {
            z.add_directory(name.as_str(), o)?;
        } else {
            z.start_file(name.as_str(), o)?;
            copy_prog(rd_file(path)?, z, done, total, name, p)?;
        }
    }
    Ok(())
}

/// Below this many input bytes ZIP creation stays single-threaded.
#[cfg(not(test))]
pub(super) const ZIP_PARALLEL_MIN: u64 = 8 << 20;
#[cfg(test)]
pub(super) const ZIP_PARALLEL_MIN: u64 = 0;

/// ZIP creation. Entries are independent, so with several threads the files are split into
/// size-balanced groups, each compressed into a temporary ZIP by its own thread, and the
/// compressed entries are then raw-copied (no recompression) into the final archive.
pub(super) fn create_zip(output: &Path, items: &[Item], opts: &CreateOptions, done: &AtomicU64, total: u64, p: &Progress) -> Result<()> {
    let mut z = zip::ZipWriter::new(wr(File::create(output)?));
    let files: Vec<&Item> = items.iter().filter(|i| !i.2).collect();
    let t = (threads() as usize).min(files.len());
    if t < 2 || total < ZIP_PARALLEL_MIN || opts.level == Some(0) {
        zip_write_items(&mut z, &items.iter().collect::<Vec<_>>(), opts, done, total, p)?;
        z.finish()?.flush()?;
        return Ok(());
    }
    // directories first, then groups of files
    let dirs: Vec<&Item> = items.iter().filter(|i| i.2).collect();
    zip_write_items(&mut z, &dirs, opts, done, total, p)?;
    // longest-processing-time-first partitioning by file size
    let mut sized: Vec<(u64, &Item)> = files.iter().map(|i| (fs::metadata(&i.0).map(|m| m.len()).unwrap_or(0), *i)).collect();
    sized.sort_by(|a, b| b.0.cmp(&a.0));
    let mut bins: Vec<(u64, Vec<(usize, &Item)>)> = (0..t).map(|_| (0, Vec::new())).collect();
    let order: std::collections::HashMap<*const Item, usize> = files.iter().enumerate().map(|(k, i)| (*i as *const Item, k)).collect();
    for (sz, it) in sized {
        let b = bins.iter_mut().min_by_key(|b| b.0).unwrap();
        b.0 += sz;
        b.1.push((order[&(it as *const Item)], it));
    }
    let tmp: Vec<PathBuf> = (0..t).map(|k| {
        let mut n = output.as_os_str().to_owned();
        n.push(format!(".part{k}.tmp"));
        PathBuf::from(n)
    }).collect();
    let res: Result<()> = std::thread::scope(|sc| {
        let hs: Vec<_> = bins.iter_mut().zip(&tmp).map(|(b, path)| {
            b.1.sort_by_key(|x| x.0); // keep original (sorted) order inside each group
            let group: Vec<&Item> = b.1.iter().map(|x| x.1).collect();
            sc.spawn(move || -> Result<()> {
                let mut w = zip::ZipWriter::new(wr(File::create(path)?));
                zip_write_items(&mut w, &group, opts, done, total, p)?;
                w.finish()?.flush()?;
                Ok(())
            })
        }).collect();
        let mut first_err = None;
        for h in hs {
            if let Err(e) = h.join().unwrap_or_else(|_| Err(Error::Io(io::Error::other("worker panicked")))) {
                if first_err.is_none() { first_err = Some(e); }
            }
        }
        first_err.map_or(Ok(()), Err)
    });
    let merged = res.and_then(|_| {
        for path in &tmp {
            p.check()?;
            z.merge_archive(zip::ZipArchive::new(BufReader::with_capacity(256 << 10, File::open(path)?))?)?;
        }
        z.finish()?.flush()?;
        Ok(())
    });
    for path in &tmp {
        let _ = fs::remove_file(path);
    }
    merged
}

/// Create archive at `output` from files/folders in `inputs`.
pub fn create(output: &Path, inputs: &[PathBuf], opts: &CreateOptions, p: &Progress) -> Result<()> {
    let fmt = opts.format;
    let items = collect(inputs)?;
    let total: u64 = items.iter().filter(|i| !i.2).map(|i| fs::metadata(&i.0).map(|m| m.len()).unwrap_or(0)).sum();
    let done = AtomicU64::new(0);
    let result = (|| -> Result<()> {
        match fmt {
            Format::Rar => Err(Error::Unsupported("RAR creation is not possible (proprietary format)".into())),
            Format::Zip => create_zip(output, &items, opts, &done, total, p),
            Format::SevenZ => {
                use sevenz_rust2::encoder_options::*;
                let mut w = sevenz_rust2::ArchiveWriter::create(output).map_err(|e| Error::SevenZ(e.to_string()))?;
                let mut methods: Vec<sevenz_rust2::EncoderConfiguration> = Vec::new();
                if let Some(pw) = opts.password.as_deref().filter(|s| !s.is_empty()) {
                    methods.push(AesEncoderOptions::new(pw.into()).into());
                    w.set_encrypt_header(true);
                }
                let lvl = opts.level.unwrap_or(6).min(9);
                let t = threads();
                methods.push(if t > 1 {
                    // independent LZMA2 chunks of 4x the dictionary size, compressed in parallel
                    const DICT: [u64; 10] = [256 << 10, 1 << 20, 2 << 20, 4 << 20, 4 << 20, 8 << 20, 8 << 20, 16 << 20, 32 << 20, 64 << 20];
                    Lzma2Options::from_level_mt(lvl, t, DICT[lvl as usize] * 4).into()
                } else {
                    Lzma2Options::from_level(lvl).into()
                });
                w.set_content_methods(methods);
                // Directories and empty files carry no data stream.
                let mut solid_entries = Vec::new();
                let mut solid_readers = Vec::new();
                for (path, name, is_dir) in &items {
                    let entry = sevenz_rust2::ArchiveEntry::from_path(path, name.clone());
                    let empty = !*is_dir && fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true);
                    if *is_dir || empty {
                        w.push_archive_entry::<File>(entry, None).map_err(|e| Error::SevenZ(e.to_string()))?;
                    } else {
                        solid_entries.push(entry);
                        solid_readers.push(sevenz_rust2::SourceReader::new(ProgReader::new(LazyFile::new(path), &done, total, name, p)));
                    }
                }
                // One solid block: better ratio, and lets multi-threaded LZMA2 split work across
                // all files instead of compressing each small file on its own.
                if !solid_entries.is_empty() {
                    w.push_archive_entries(solid_entries, solid_readers).map_err(|e| {
                        if p.cancel.load(Ordering::Relaxed) { Error::Cancelled } else { Error::SevenZ(e.to_string()) }
                    })?;
                }
                w.finish()?;
                Ok(())
            }
            f if f.is_single_file() => {
                let files: Vec<_> = items.iter().filter(|i| !i.2).collect();
                if files.len() != 1 {
                    return Err(Error::Unsupported(format!("{} can only compress exactly one file; use TAR.{} for multiple files/folders", f.label(), f.extension().to_uppercase())));
                }
                let mut enc = encoder(f, wr(File::create(output)?), opts.level)?;
                copy_prog(rd(File::open(&files[0].0)?), &mut enc, &done, total, &files[0].1, p)?;
                enc.finish_box()?;
                Ok(())
            }
            f => {
                let enc = encoder(f, wr(File::create(output)?), opts.level)?;
                let mut t = tar::Builder::new(enc);
                t.follow_symlinks(false);
                for (path, name, is_dir) in &items {
                    p.check()?;
                    if *is_dir {
                        t.append_dir(name, path)?;
                    } else {
                        let file = File::open(path)?;
                        let mut h = tar::Header::new_gnu();
                        h.set_metadata(&file.metadata()?);
                        let pr = ProgReader::new(rd(file), &done, total, name, p);
                        t.append_data(&mut h, name, pr).map_err(|e| {
                            if p.cancel.load(Ordering::Relaxed) { Error::Cancelled } else { Error::Io(e) }
                        })?;
                    }
                }
                t.into_inner()?.finish_box()?;
                Ok(())
            }
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(output);
    }
    result
}

/// Suggest an output file name for a new archive given the inputs.
pub fn suggest_output(inputs: &[PathBuf], fmt: Format) -> PathBuf {
    let first = inputs.first().cloned().unwrap_or_else(|| PathBuf::from("archive"));
    let dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
    let base = if inputs.len() == 1 {
        first.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "archive".into())
    } else {
        "archive".into()
    };
    dir.join(format!("{base}.{}", fmt.extension()))
}
