//! archiver-core: format detection, listing, extraction and creation of archives.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

const BUF: usize = 1 << 20;
/// Large buffered reader (fewer syscalls than the 8 KiB default).
fn rd<R: Read>(r: R) -> BufReader<R> {
    BufReader::with_capacity(BUF, r)
}
fn wr<W: Write>(w: W) -> BufWriter<W> {
    BufWriter::with_capacity(BUF, w)
}

/// Worker threads for multi-threaded codecs (override with ARCHIVER_THREADS).
pub fn threads() -> u32 {
    std::env::var("ARCHIVER_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &u32| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1))
        .min(64)
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
        Format::TarXz | Format::Xz => Box::new(XzMulti::new(r)?),
        Format::TarZst | Format::Zst => Box::new(zstd::stream::read::Decoder::new(r)?),
        f => return Err(Error::Unsupported(f.label().into())),
    })
}

fn encoder(fmt: Format, w: impl Write + 'static, level: Option<u32>) -> Result<Box<dyn FinishWrite>> {
    let lvl = level.or(fmt.level_range().map(|r| r.2)).unwrap_or(6);
    Ok(match fmt {
        Format::Tar => Box::new(Plain(w)),
        Format::TarGz | Format::Gz => {
            let t = threads() as usize;
            if t > 1 {
                Box::new(ParGzWriter::new(w, lvl.min(9), t))
            } else {
                Box::new(flate2::write::GzEncoder::new(w, flate2::Compression::new(lvl.min(9))))
            }
        }
        Format::TarBz2 | Format::Bz2 => {
            Box::new(bzip2::write::BzEncoder::new(w, bzip2::Compression::new(lvl.clamp(1, 9))))
        }
        Format::TarXz | Format::Xz => {
            let stream = liblzma::stream::MtStreamBuilder::new()
                .preset(lvl.min(9))
                .check(liblzma::stream::Check::Crc64)
                .threads(threads())
                // let liblzma drop threads if they would need more than ~1 GiB
                .memlimit_threading(1 << 30)
                .encoder()
                .map_err(io::Error::from)?;
            Box::new(liblzma::write::XzEncoder::new_stream(w, stream))
        }
        Format::TarZst | Format::Zst => {
            let mut e = zstd::stream::write::Encoder::new(w, lvl.clamp(1, 22) as i32)?;
            let t = threads();
            if t > 1 {
                e.multithread(t)?;
            }
            Box::new(e)
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

/// Opens the file on first read and closes it at EOF (keeps few descriptors open
/// while thousands of files are queued for a solid 7z block).
struct LazyFile {
    path: PathBuf,
    f: Option<BufReader<File>>,
    finished: bool,
}
impl LazyFile {
    fn new(path: &Path) -> Self {
        Self { path: path.to_path_buf(), f: None, finished: false }
    }
}
impl Read for LazyFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.finished {
            return Ok(0);
        }
        if self.f.is_none() {
            let f = File::open(&self.path)?;
            let len = f.metadata().map(|m| m.len()).unwrap_or(0) as usize;
            self.f = Some(BufReader::with_capacity(len.clamp(8 << 10, BUF), f));
        }
        let n = self.f.as_mut().unwrap().read(buf)?;
        if n == 0 && !buf.is_empty() {
            self.f = None;
            self.finished = true;
        }
        Ok(n)
    }
}

/// Parallel gzip: input is cut into fixed blocks, each compressed on its own thread into a
/// complete gzip member; members are written in order. The result is a standard multi-member
/// gzip file (RFC 1952) readable by gzip/zcat/7-Zip; ratio loss is tiny for 4 MiB blocks.
struct ParGzWriter<W: Write> {
    out: W,
    level: u32,
    max_inflight: usize,
    buf: Vec<u8>,
    inflight: std::collections::VecDeque<std::thread::JoinHandle<io::Result<Vec<u8>>>>,
}
const GZ_BLOCK: usize = 4 << 20;
impl<W: Write> ParGzWriter<W> {
    fn new(out: W, level: u32, threads: usize) -> Self {
        Self { out, level, max_inflight: threads, buf: Vec::with_capacity(GZ_BLOCK), inflight: Default::default() }
    }
    fn dispatch(&mut self) -> io::Result<()> {
        let block = std::mem::replace(&mut self.buf, Vec::with_capacity(GZ_BLOCK));
        let level = self.level;
        self.inflight.push_back(std::thread::spawn(move || {
            let mut e = flate2::write::GzEncoder::new(Vec::with_capacity(block.len() / 2), flate2::Compression::new(level));
            e.write_all(&block)?;
            e.finish()
        }));
        while self.inflight.len() >= self.max_inflight {
            self.drain_one()?;
        }
        Ok(())
    }
    fn drain_one(&mut self) -> io::Result<()> {
        if let Some(h) = self.inflight.pop_front() {
            let data = h.join().map_err(|_| io::Error::other("gzip worker panicked"))??;
            self.out.write_all(&data)?;
        }
        Ok(())
    }
}
impl<W: Write> Write for ParGzWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = data.len().min(GZ_BLOCK - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == GZ_BLOCK {
            self.dispatch()?;
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl<W: Write> FinishWrite for ParGzWriter<W> {
    fn finish_box(mut self: Box<Self>) -> io::Result<()> {
        if !self.buf.is_empty() || self.inflight.is_empty() {
            self.dispatch()?;
        }
        while !self.inflight.is_empty() {
            self.drain_one()?;
        }
        self.out.flush()
    }
}

/// Reader wrapper reporting progress (throttled to every 1 MiB to keep callbacks cheap).
struct ProgReader<'a, 'b, R> {
    inner: R,
    done: &'b AtomicU64,
    total: u64,
    name: String,
    p: &'b Progress<'a>,
    pending: u64,
}
const REPORT_EVERY: u64 = 1 << 20;
impl<'a, 'b, R> ProgReader<'a, 'b, R> {
    fn new(inner: R, done: &'b AtomicU64, total: u64, name: &str, p: &'b Progress<'a>) -> Self {
        Self { inner, done, total, name: name.to_string(), p, pending: 0 }
    }
    fn flush_progress(&mut self) {
        if self.pending > 0 {
            let d = self.done.fetch_add(self.pending, Ordering::Relaxed) + self.pending;
            self.pending = 0;
            (self.p.on)(d, self.total, &self.name);
        }
    }
}
impl<R: Read> Read for ProgReader<'_, '_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pending += n as u64;
        if n == 0 || self.pending >= REPORT_EVERY {
            if self.p.cancel.load(Ordering::Relaxed) {
                return Err(io::Error::other("cancelled"));
            }
            self.flush_progress();
        }
        Ok(n)
    }
}
impl<R> Drop for ProgReader<'_, '_, R> {
    fn drop(&mut self) {
        self.flush_progress();
    }
}

fn copy_prog(r: impl Read, w: &mut impl Write, done: &AtomicU64, total: u64, name: &str, p: &Progress) -> Result<()> {
    let mut pr = ProgReader::new(r, done, total, name, p);
    match io::copy(&mut pr, w) {
        Ok(_) => Ok(()),
        Err(_) if p.cancel.load(Ordering::Relaxed) => Err(Error::Cancelled),
        Err(e) => Err(e.into()),
    }
}

/// Multi-threaded xz decoder that also handles concatenated .xz streams
/// (liblzma's MT decoder stops after the first stream).
struct XzMulti<R: Read> {
    dec: Option<liblzma::bufread::XzDecoder<BufReader<R>>>,
}
impl<R: Read> XzMulti<R> {
    fn new(r: R) -> io::Result<Self> {
        Ok(Self { dec: Some(Self::mk(rd(r))?) })
    }
    fn mk(r: BufReader<R>) -> io::Result<liblzma::bufread::XzDecoder<BufReader<R>>> {
        let s = liblzma::stream::MtStreamBuilder::new()
            .threads(threads())
            .memlimit_threading(1 << 30)
            .memlimit_stop(u64::MAX)
            .decoder()
            .map_err(io::Error::from)?;
        Ok(liblzma::bufread::XzDecoder::new_stream(r, s))
    }
}
impl<R: Read> Read for XzMulti<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use io::BufRead;
        loop {
            let Some(d) = self.dec.as_mut() else { return Ok(0) };
            let n = d.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            // end of one stream: skip stream padding (NUL bytes) and continue if more data follows
            let mut inner = self.dec.take().unwrap().into_inner();
            loop {
                let b = inner.fill_buf()?;
                if b.is_empty() {
                    return Ok(0);
                }
                let z = b.iter().take_while(|&&c| c == 0).count();
                let all = z == b.len();
                inner.consume(z);
                if !all {
                    break;
                }
            }
            self.dec = Some(Self::mk(inner)?);
        }
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
            let mut d = decoder(f, rd(File::open(path)?))?;
            let size = io::copy(&mut d, &mut io::sink())?;
            out.push(Entry { name: single_inner_name(path), size, compressed: Some(compressed), modified: None, is_dir: false });
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
        Format::Zip => {
            let mut z = zip::ZipArchive::new(BufReader::with_capacity(64 << 10, File::open(archive)?))?;
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
                    let mut w = wr(File::create(&out)?);
                    copy_prog(&mut f, &mut w, &done, total, &name, p)?;
                }
            }
        }
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

fn zip_options<'a>(path: &Path, opts: &'a CreateOptions) -> zip::write::FileOptions<'a, 'static, ()> {
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
fn rd_file(path: &Path) -> Result<BufReader<File>> {
    let f = File::open(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0) as usize;
    Ok(BufReader::with_capacity(len.clamp(8 << 10, BUF), f))
}

type Item = (PathBuf, String, bool);

fn zip_write_items<W: Write + io::Seek>(z: &mut zip::ZipWriter<W>, items: &[&Item], opts: &CreateOptions, done: &AtomicU64, total: u64, p: &Progress) -> Result<()> {
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
const ZIP_PARALLEL_MIN: u64 = 8 << 20;
#[cfg(test)]
const ZIP_PARALLEL_MIN: u64 = 0;

/// ZIP creation. Entries are independent, so with several threads the files are split into
/// size-balanced groups, each compressed into a temporary ZIP by its own thread, and the
/// compressed entries are then raw-copied (no recompression) into the final archive.
fn create_zip(output: &Path, items: &[Item], opts: &CreateOptions, done: &AtomicU64, total: u64, p: &Progress) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(dir: &Path) -> PathBuf {
        let root = dir.join("data");
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::write(root.join("hello.txt"), b"hello world\n".repeat(100)).unwrap();
        fs::write(root.join("sub/a.bin"), (0..50_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>()).unwrap();
        fs::write(root.join("sub/deeper/中文.txt"), "測試內容".as_bytes()).unwrap();
        fs::write(root.join("sub/empty.txt"), b"").unwrap();
        fs::create_dir_all(root.join("emptydir")).unwrap();
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
    fn cancel_removes_partial_output() {
        for fmt in [Format::Zip, Format::SevenZ, Format::TarGz, Format::TarXz, Format::TarZst] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            let out = t.path().join(format!("c.{}", fmt.extension()));
            let p = Progress { on: &|_, _, _| {}, cancel: Arc::new(AtomicBool::new(true)) };
            let r = create(&out, &[src], &CreateOptions { format: fmt, level: None, password: None }, &p);
            assert!(matches!(r, Err(Error::Cancelled)), "{fmt:?}: {r:?}");
            assert!(!out.exists(), "{fmt:?}");
            assert_eq!(fs::read_dir(t.path()).unwrap().count(), 1, "{fmt:?}: temp files left");
        }
    }

    #[test]
    fn parallel_gzip_multi_member() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("big.bin");
        let data: Vec<u8> = (0..(GZ_BLOCK * 2 + 12345)).map(|i| ((i * 31) ^ (i >> 7)) as u8).collect();
        fs::write(&f, &data).unwrap();
        let out = t.path().join("big.bin.gz");
        create(&out, &[f], &CreateOptions { format: Format::Gz, level: Some(6), password: None }, &Progress::none()).unwrap();
        // standard decoder must read all members
        let mut d = flate2::read::MultiGzDecoder::new(File::open(&out).unwrap());
        let mut back = Vec::new();
        d.read_to_end(&mut back).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn xz_concatenated_streams() {
        let t = tempfile::tempdir().unwrap();
        let mut all = Vec::new();
        let mut want = Vec::new();
        for part in [b"first part ".repeat(1000), b"second part ".repeat(2000)] {
            let f = t.path().join("p.txt");
            fs::write(&f, &part).unwrap();
            let x = t.path().join("p.txt.xz");
            create(&x, &[f], &CreateOptions { format: Format::Xz, level: Some(1), password: None }, &Progress::none()).unwrap();
            all.extend(fs::read(&x).unwrap());
            all.extend([0u8; 4]); // stream padding
            want.extend(part);
        }
        let cat = t.path().join("cat.txt.xz");
        fs::write(&cat, &all).unwrap();
        let dest = t.path().join("out");
        extract(&cat, &dest, None, None, &Progress::none()).unwrap();
        assert_eq!(fs::read(dest.join("cat.txt")).unwrap(), want);
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
