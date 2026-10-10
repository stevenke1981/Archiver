//! archiver-core: format detection, listing, extraction and creation of archives.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use walkdir::WalkDir;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("7z error: {0}")]
    SevenZ(String),
    #[error("RAR error: {0}")]
    Rar(String),
    #[error("unsupported format: {0}")]
    Unsupported(String),
    #[error("operation cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
    Gz,
    Bz2,
    Xz,
    Zst,
    Rar,
}

impl Format {
    pub const ALL: [Format; 12] = [
        Format::Zip, Format::SevenZ, Format::Tar, Format::TarGz, Format::TarBz2, Format::TarXz,
        Format::TarZst, Format::Gz, Format::Bz2, Format::Xz, Format::Zst, Format::Rar,
    ];

    pub fn writable() -> Vec<Format> {
        Self::ALL.into_iter().filter(|f| f.can_create()).collect()
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::SevenZ => "7z",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::TarBz2 => "tar.bz2",
            Format::TarXz => "tar.xz",
            Format::TarZst => "tar.zst",
            Format::Gz => "gz",
            Format::Bz2 => "bz2",
            Format::Xz => "xz",
            Format::Zst => "zst",
            Format::Rar => "rar",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "ZIP",
            Format::SevenZ => "7z",
            Format::Tar => "TAR",
            Format::TarGz => "TAR.GZ",
            Format::TarBz2 => "TAR.BZ2",
            Format::TarXz => "TAR.XZ",
            Format::TarZst => "TAR.ZST",
            Format::Gz => "GZ (single file)",
            Format::Bz2 => "BZ2 (single file)",
            Format::Xz => "XZ (single file)",
            Format::Zst => "ZST (single file)",
            Format::Rar => "RAR (extract only)",
        }
    }

    pub fn can_create(self) -> bool {
        self != Format::Rar
    }
    pub fn supports_password(self) -> bool {
        matches!(self, Format::Zip | Format::SevenZ | Format::Rar)
    }
    pub fn is_single_file(self) -> bool {
        matches!(self, Format::Gz | Format::Bz2 | Format::Xz | Format::Zst)
    }
    /// Supported compression level range (min, max, default), None if not configurable.
    pub fn level_range(self) -> Option<(u32, u32, u32)> {
        match self {
            Format::Tar | Format::Rar => None,
            Format::TarZst | Format::Zst => Some((1, 22, 3)),
            _ => Some((0, 9, 6)),
        }
    }

    /// Detect format from a file name (by extension).
    pub fn detect(path: &Path) -> Option<Format> {
        let n = path.file_name()?.to_string_lossy().to_lowercase();
        EXT_TABLE.iter().find(|(e, _)| n.ends_with(e)).map(|(_, f)| *f)
    }
}

/// File name without its archive extension (`foo.tar.gz` -> `foo`).
pub fn archive_stem(path: &Path) -> String {
    let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let lower = name.to_lowercase();
    match EXT_TABLE.iter().find(|(e, _)| lower.ends_with(e)) {
        Some((e, _)) if name.len() > e.len() => name[..name.len() - e.len()].to_string(),
        _ => name,
    }
}

/// All recognised archive file extensions (without leading dot), e.g. for shell registration.
pub fn archive_extensions() -> Vec<&'static str> {
    EXT_TABLE.iter().map(|(e, _)| e.trim_start_matches('.')).collect()
}

const EXT_TABLE: &[(&str, Format)] = &[
            (".tar.gz", Format::TarGz), (".tgz", Format::TarGz),
            (".tar.bz2", Format::TarBz2), (".tbz2", Format::TarBz2), (".tbz", Format::TarBz2),
            (".tar.xz", Format::TarXz), (".txz", Format::TarXz),
            (".tar.zst", Format::TarZst), (".tzst", Format::TarZst),
            (".tar", Format::Tar), (".zip", Format::Zip), (".jar", Format::Zip),
            (".7z", Format::SevenZ), (".rar", Format::Rar),
            (".gz", Format::Gz), (".bz2", Format::Bz2), (".xz", Format::Xz), (".zst", Format::Zst),
];

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub compressed: Option<u64>,
    pub modified: Option<String>,
    pub is_dir: bool,
}

/// Progress callback: (bytes_done, bytes_total, current item name). Shared cancel flag.
pub struct Progress<'a> {
    pub on: &'a (dyn Fn(u64, u64, &str) + Sync),
    pub cancel: Arc<AtomicBool>,
}

impl<'a> Progress<'a> {
    pub fn none() -> Progress<'static> {
        static NOOP: fn(u64, u64, &str) = |_, _, _| {};
        Progress { on: &NOOP, cancel: Arc::new(AtomicBool::new(false)) }
    }
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) }
    }
}

fn fmt_time(secs: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(secs, 0).map(|d| {
        d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()
    })
}

fn detect_or_err(path: &Path) -> Result<Format> {
    Format::detect(path).ok_or_else(|| Error::Unsupported(path.display().to_string()))
}

fn single_inner_name(archive: &Path) -> String {
    let stem = archive.file_stem().map(|s| s.to_string_lossy().to_string());
    stem.unwrap_or_else(|| "output".into())
}

/// Sanitize an in-archive path so it stays inside `dest` (zip-slip protection).
fn safe_join(dest: &Path, name: &str) -> Option<PathBuf> {
    let mut out = dest.to_path_buf();
    for c in Path::new(&name.replace('\\', "/")).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn wanted(selected: Option<&[String]>, name: &str) -> bool {
    match selected {
        None => true,
        Some(sel) => {
            let n = name.trim_end_matches('/');
            sel.iter().any(|s| {
                let s = s.trim_end_matches('/');
                n == s || n.starts_with(&format!("{s}/"))
            })
        }
    }
}

// ---------------------------------------------------------------- decoders

fn decoder(fmt: Format, r: impl Read + 'static) -> Result<Box<dyn Read>> {
    Ok(match fmt {
        Format::Tar => Box::new(r),
        Format::TarGz | Format::Gz => Box::new(flate2::read::MultiGzDecoder::new(r)),
        Format::TarBz2 | Format::Bz2 => Box::new(bzip2::read::MultiBzDecoder::new(r)),
        Format::TarXz | Format::Xz => Box::new(liblzma::read::XzDecoder::new_multi_decoder(r)),
        Format::TarZst | Format::Zst => Box::new(zstd::stream::read::Decoder::new(r)?),
        f => return Err(Error::Unsupported(f.label().into())),
    })
}

fn encoder(fmt: Format, w: impl Write + 'static, level: Option<u32>) -> Result<Box<dyn FinishWrite>> {
    let lvl = level.or(fmt.level_range().map(|r| r.2)).unwrap_or(6);
    Ok(match fmt {
        Format::Tar => Box::new(Plain(w)),
        Format::TarGz | Format::Gz => {
            Box::new(flate2::write::GzEncoder::new(w, flate2::Compression::new(lvl.min(9))))
        }
        Format::TarBz2 | Format::Bz2 => {
            Box::new(bzip2::write::BzEncoder::new(w, bzip2::Compression::new(lvl.clamp(1, 9))))
        }
        Format::TarXz | Format::Xz => Box::new(liblzma::write::XzEncoder::new(w, lvl.min(9))),
        Format::TarZst | Format::Zst => {
            Box::new(zstd::stream::write::Encoder::new(w, lvl.clamp(1, 22) as i32)?)
        }
        f => return Err(Error::Unsupported(f.label().into())),
    })
}

trait FinishWrite: Write {
    fn finish_box(self: Box<Self>) -> io::Result<()>;
}
struct Plain<W: Write>(W);
impl<W: Write> Write for Plain<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> { self.0.write(b) }
    fn flush(&mut self) -> io::Result<()> { self.0.flush() }
}
impl<W: Write> FinishWrite for Plain<W> {
    fn finish_box(mut self: Box<Self>) -> io::Result<()> { self.0.flush() }
}
impl<W: Write> FinishWrite for flate2::write::GzEncoder<W> {
    fn finish_box(self: Box<Self>) -> io::Result<()> { self.finish()?.flush() }
}
impl<W: Write> FinishWrite for bzip2::write::BzEncoder<W> {
    fn finish_box(self: Box<Self>) -> io::Result<()> { self.finish()?.flush() }
}
impl<W: Write> FinishWrite for liblzma::write::XzEncoder<W> {
    fn finish_box(self: Box<Self>) -> io::Result<()> { self.finish()?.flush() }
}
impl<W: Write> FinishWrite for zstd::stream::write::Encoder<'static, W> {
    fn finish_box(self: Box<Self>) -> io::Result<()> { self.finish()?.flush() }
}

/// Reader wrapper reporting progress.
struct ProgReader<'a, 'b, R> {
    inner: R,
    done: &'b mut u64,
    total: u64,
    name: String,
    p: &'b Progress<'a>,
}
impl<R: Read> Read for ProgReader<'_, '_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.p.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("cancelled"));
        }
        let n = self.inner.read(buf)?;
        *self.done += n as u64;
        (self.p.on)(*self.done, self.total, &self.name);
        Ok(n)
    }
}

fn copy_prog(r: impl Read, w: &mut impl Write, done: &mut u64, total: u64, name: &str, p: &Progress) -> Result<()> {
    let mut pr = ProgReader { inner: r, done, total, name: name.to_string(), p };
    match io::copy(&mut pr, w) {
        Ok(_) => Ok(()),
        Err(_) if p.cancel.load(Ordering::Relaxed) => Err(Error::Cancelled),
        Err(e) => Err(e.into()),
    }
}

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
                });
            }
        }
        Format::SevenZ => {
            let pw = password.map(sevenz_rust2::Password::from).unwrap_or_else(sevenz_rust2::Password::empty);
            let r = sevenz_rust2::ArchiveReader::open(path, pw).map_err(|e| Error::SevenZ(e.to_string()))?;
            for e in &r.archive().files {
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
                });
            }
        }
        f if f.is_single_file() => {
            let compressed = fs::metadata(path)?.len();
            let mut d = decoder(f, BufReader::new(File::open(path)?))?;
            let size = io::copy(&mut d, &mut io::sink())?;
            out.push(Entry { name: single_inner_name(path), size, compressed: Some(compressed), modified: None, is_dir: false });
        }
        f => {
            let mut a = tar::Archive::new(decoder(f, BufReader::new(File::open(path)?))?);
            for e in a.entries()? {
                let e = e?;
                let h = e.header();
                out.push(Entry {
                    name: e.path()?.to_string_lossy().to_string(),
                    size: h.size().unwrap_or(0),
                    compressed: None,
                    modified: h.mtime().ok().and_then(|t| fmt_time(t as i64)),
                    is_dir: h.entry_type().is_dir(),
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
    let mut done = 0u64;
    match fmt {
        Format::Zip => {
            let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive)?))?;
            let mut total = 0;
            for i in 0..z.len() {
                let f = z.by_index_raw(i)?;
                if wanted(selected, &f.name()?) { total += f.size(); }
            }
            for i in 0..z.len() {
                p.check()?;
                let mut f = match password {
                    Some(pw) => z.by_index_decrypt(i, pw.as_bytes())?,
                    None => z.by_index(i)?,
                };
                let name = f.name()?.to_string();
                if !wanted(selected, &name) { continue; }
                let Some(rel) = f.enclosed_name() else { continue };
                let out = dest.join(rel);
                if f.is_dir() {
                    fs::create_dir_all(&out)?;
                } else {
                    if let Some(par) = out.parent() { fs::create_dir_all(par)?; }
                    let mut w = BufWriter::new(File::create(&out)?);
                    copy_prog(&mut f, &mut w, &mut done, total, &name, p)?;
                }
            }
        }
        Format::SevenZ => {
            let pw = password.map(sevenz_rust2::Password::from).unwrap_or_else(sevenz_rust2::Password::empty);
            let mut r = sevenz_rust2::ArchiveReader::open(archive, pw).map_err(|e| Error::SevenZ(e.to_string()))?;
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
                        let mut w = BufWriter::new(File::create(&out)?);
                        copy_prog(rd, &mut w, &mut done, total, &e.name, p)?;
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
                    done += size;
                    (p.on)(done, total, &name);
                    c
                } else {
                    h.skip().map_err(|e| Error::Rar(e.to_string()))?
                };
            }
        }
        f if f.is_single_file() => {
            let total = fs::metadata(archive)?.len();
            let name = single_inner_name(archive);
            let d = decoder(f, BufReader::new(File::open(archive)?))?;
            let mut w = BufWriter::new(File::create(dest.join(&name))?);
            // progress is approximate (decompressed bytes vs compressed size)
            copy_prog(d, &mut w, &mut done, total.max(1), &name, p)?;
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

struct CountingReader<R> { inner: R, count: Arc<std::sync::atomic::AtomicU64> }
impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(b)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

// ---------------------------------------------------------------- creation

#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub format: Format,
    /// Compression level (see Format::level_range); None = default.
    pub level: Option<u32>,
    pub password: Option<String>,
}

/// Expand inputs into (absolute path, archive name, is_dir).
fn collect(inputs: &[PathBuf]) -> Result<Vec<(PathBuf, String, bool)>> {
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

/// Create archive at `output` from files/folders in `inputs`.
pub fn create(output: &Path, inputs: &[PathBuf], opts: &CreateOptions, p: &Progress) -> Result<()> {
    let fmt = opts.format;
    let items = collect(inputs)?;
    let total: u64 = items.iter().filter(|i| !i.2).map(|i| fs::metadata(&i.0).map(|m| m.len()).unwrap_or(0)).sum();
    let mut done = 0u64;
    let result = (|| -> Result<()> {
        match fmt {
            Format::Rar => Err(Error::Unsupported("RAR creation is not possible (proprietary format)".into())),
            Format::Zip => {
                let mut z = zip::ZipWriter::new(BufWriter::new(File::create(output)?));
                let lvl = opts.level.unwrap_or(6);
                for (path, name, is_dir) in &items {
                    p.check()?;
                    let mut o = zip::write::SimpleFileOptions::default()
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
                    if *is_dir {
                        z.add_directory(name.as_str(), o)?;
                    } else {
                        z.start_file(name.as_str(), o)?;
                        copy_prog(BufReader::new(File::open(path)?), &mut z, &mut done, total, name, p)?;
                    }
                }
                z.finish()?.flush()?;
                Ok(())
            }
            Format::SevenZ => {
                use sevenz_rust2::encoder_options::*;
                let mut w = sevenz_rust2::ArchiveWriter::create(output).map_err(|e| Error::SevenZ(e.to_string()))?;
                let mut methods: Vec<sevenz_rust2::EncoderConfiguration> = Vec::new();
                if let Some(pw) = opts.password.as_deref().filter(|s| !s.is_empty()) {
                    methods.push(AesEncoderOptions::new(pw.into()).into());
                    w.set_encrypt_header(true);
                }
                methods.push(Lzma2Options::from_level(opts.level.unwrap_or(6).min(9)).into());
                w.set_content_methods(methods);
                for (path, name, is_dir) in &items {
                    p.check()?;
                    let entry = sevenz_rust2::ArchiveEntry::from_path(path, name.clone());
                    if *is_dir {
                        w.push_archive_entry::<File>(entry, None).map_err(|e| Error::SevenZ(e.to_string()))?;
                    } else {
                        let pr = ProgReader { inner: BufReader::new(File::open(path)?), done: &mut done, total, name: name.clone(), p };
                        w.push_archive_entry(entry, Some(pr)).map_err(|e| {
                            if p.cancel.load(Ordering::Relaxed) { Error::Cancelled } else { Error::SevenZ(e.to_string()) }
                        })?;
                    }
                }
                w.finish()?;
                Ok(())
            }
            f if f.is_single_file() => {
                let files: Vec<_> = items.iter().filter(|i| !i.2).collect();
                if files.len() != 1 {
                    return Err(Error::Unsupported(format!("{} can only compress exactly one file; use TAR.{} for multiple files/folders", f.label(), f.extension().to_uppercase())));
                }
                let mut enc = encoder(f, BufWriter::new(File::create(output)?), opts.level)?;
                copy_prog(BufReader::new(File::open(&files[0].0)?), &mut enc, &mut done, total, &files[0].1, p)?;
                enc.finish_box()?;
                Ok(())
            }
            f => {
                let enc = encoder(f, BufWriter::new(File::create(output)?), opts.level)?;
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
                        let pr = ProgReader { inner: BufReader::new(file), done: &mut done, total, name: name.clone(), p };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(dir: &Path) -> PathBuf {
        let root = dir.join("data");
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::write(root.join("hello.txt"), b"hello world\n".repeat(100)).unwrap();
        fs::write(root.join("sub/a.bin"), (0..50_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>()).unwrap();
        fs::write(root.join("sub/deeper/中文.txt"), "測試內容".as_bytes()).unwrap();
        root
    }

    fn assert_same(a: &Path, b: &Path) {
        for e in WalkDir::new(a) {
            let e = e.unwrap();
            let rel = e.path().strip_prefix(a).unwrap();
            let other = b.join(rel);
            if e.file_type().is_file() {
                assert_eq!(fs::read(e.path()).unwrap(), fs::read(&other).unwrap(), "{rel:?}");
            } else {
                assert!(other.is_dir(), "{rel:?} missing");
            }
        }
    }

    fn roundtrip(fmt: Format, password: Option<&str>) {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join(format!("out.{}", fmt.extension()));
        let opts = CreateOptions { format: fmt, level: None, password: password.map(String::from) };
        create(&out, &[src.clone()], &opts, &Progress::none()).unwrap();
        assert_eq!(Format::detect(&out), Some(fmt));
        let entries = list(&out, password).unwrap();
        assert!(entries.iter().any(|e| e.name.ends_with("hello.txt")), "{entries:?}");
        let dest = t.path().join("x");
        extract(&out, &dest, password, None, &Progress::none()).unwrap();
        assert_same(&src, &dest.join("data"));

        // selective extraction
        let dest2 = t.path().join("y");
        extract(&out, &dest2, password, Some(&["data/sub".to_string()]), &Progress::none()).unwrap();
        assert!(dest2.join("data/sub/a.bin").exists());
        assert!(!dest2.join("data/hello.txt").exists());
    }

    #[test] fn zip() { roundtrip(Format::Zip, None) }
    #[test] fn zip_password() { roundtrip(Format::Zip, Some("s3cret")) }
    #[test] fn sevenz() { roundtrip(Format::SevenZ, None) }
    #[test] fn sevenz_password() { roundtrip(Format::SevenZ, Some("s3cret")) }
    #[test] fn tar() { roundtrip(Format::Tar, None) }
    #[test] fn tar_gz() { roundtrip(Format::TarGz, None) }
    #[test] fn tar_bz2() { roundtrip(Format::TarBz2, None) }
    #[test] fn tar_xz() { roundtrip(Format::TarXz, None) }
    #[test] fn tar_zst() { roundtrip(Format::TarZst, None) }

    #[test]
    fn zip_wrong_password_fails() {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join("p.zip");
        create(&out, &[src], &CreateOptions { format: Format::Zip, level: Some(9), password: Some("right".into()) }, &Progress::none()).unwrap();
        assert!(extract(&out, &t.path().join("x"), Some("wrong"), None, &Progress::none()).is_err());
    }

    #[test]
    fn single_files() {
        for fmt in [Format::Gz, Format::Bz2, Format::Xz, Format::Zst] {
            let t = tempfile::tempdir().unwrap();
            let f = t.path().join("note.txt");
            fs::write(&f, b"single file payload ".repeat(500)).unwrap();
            let out = t.path().join(format!("note.txt.{}", fmt.extension()));
            create(&out, &[f.clone()], &CreateOptions { format: fmt, level: Some(fmt.level_range().unwrap().1), password: None }, &Progress::none()).unwrap();
            let l = list(&out, None).unwrap();
            assert_eq!(l[0].name, "note.txt");
            assert_eq!(l[0].size, 10_000);
            let dest = t.path().join("x");
            extract(&out, &dest, None, None, &Progress::none()).unwrap();
            assert_eq!(fs::read(dest.join("note.txt")).unwrap(), fs::read(&f).unwrap(), "{fmt:?}");
        }
    }

    #[test]
    fn single_file_rejects_folder() {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join("x.gz");
        assert!(create(&out, &[src], &CreateOptions { format: Format::Gz, level: None, password: None }, &Progress::none()).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn rar_create_unsupported() {
        assert!(!Format::Rar.can_create());
        assert!(!Format::writable().contains(&Format::Rar));
    }

    #[test]
    fn detect_formats() {
        assert_eq!(Format::detect(Path::new("a.TGZ")), Some(Format::TarGz));
        assert_eq!(Format::detect(Path::new("a.tar.zst")), Some(Format::TarZst));
        assert_eq!(Format::detect(Path::new("a.rar")), Some(Format::Rar));
        assert_eq!(Format::detect(Path::new("a.txt")), None);
    }

    #[test]
    fn stems() {
        assert_eq!(archive_stem(Path::new("/x/My.Backup.tar.GZ")), "My.Backup");
        assert_eq!(archive_stem(Path::new("a.zip")), "a");
        assert_eq!(archive_stem(Path::new("notes.txt")), "notes.txt");
        assert!(archive_extensions().contains(&"tar.zst"));
    }

    #[test]
    fn safe_join_blocks_traversal() {
        assert!(safe_join(Path::new("/d"), "../etc/passwd").is_none());
        assert!(safe_join(Path::new("/d"), "/abs").is_none());
        assert_eq!(safe_join(Path::new("/d"), "a/b").unwrap(), Path::new("/d/a/b"));
    }
}
