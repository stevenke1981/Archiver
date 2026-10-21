//! archiver-core: format detection, listing, extraction and creation of archives.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use walkdir::WalkDir;

mod batch;
mod preview;
pub use batch::{BatchDest, BatchResult, batch_dest, batch_output, create_each, extract_each};
pub use preview::{EntryData, read_entry};

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
    #[error("entry not found: {0}")]
    NotFound(String),
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
    /// CRC32 of the uncompressed data, when the format stores it (ZIP, 7z, RAR).
    pub crc32: Option<u32>,
    /// Entry data is encrypted (ZIP/RAR, 7z with an AES coder).
    pub encrypted: bool,
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
            let n = norm_name(name);
            sel.iter().any(|s| {
                let s = norm_name(s);
                n == s || n.starts_with(&format!("{s}/"))
            })
        }
    }
}

/// Entry name as shown to the user: '/' separators, no leading "./" or '/', no trailing '/'
/// (`tar czf x.tgz .` stores "./a.txt"; selections and previews use "a.txt").
fn norm_name(name: &str) -> String {
    let n = name.replace('\\', "/");
    n.trim_start_matches("./").trim_start_matches('/').trim_end_matches('/').to_string()
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

fn open_zip(archive: &Path) -> Result<zip::ZipArchive<BufReader<File>>> {
    Ok(zip::ZipArchive::new(BufReader::with_capacity(64 << 10, File::open(archive)?))?)
}

/// ZIP extraction. Entries are chosen from the central directory alone, so unselected entries are
/// never opened or decrypted. Large jobs are split into size-balanced groups (longest first),
/// each extracted by its own thread with its own file handle.
fn extract_zip(archive: &Path, dest: &Path, password: Option<&str>, selected: Option<&[String]>, threads: usize, p: &Progress) -> Result<()> {
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
fn zip_extract_files<R: Read + io::Seek>(z: &mut zip::ZipArchive<R>, dest: &Path, password: Option<&str>, idx: &[usize], done: &AtomicU64, total: u64, p: &Progress, stop: &AtomicBool) -> Result<()> {
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
fn zip_mtime(d: zip::DateTime) -> Option<std::time::SystemTime> {
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

fn tempdir_near(archive: &Path) -> Result<PathBuf> {
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
    fn dot_slash_names_match_selection_and_preview() {
        // `tar czf x.tgz .` stores "./a.txt"; the GUI shows and selects "a.txt"
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("dot.tar.gz");
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(File::create(&out).unwrap(), flate2::Compression::default()));
        for (n, d) in [("./a.txt", &b"alpha"[..]), ("./d/b.txt", &b"beta"[..])] {
            let mut h = tar::Header::new_gnu();
            h.set_size(d.len() as u64);
            h.set_mode(0o644);
            h.as_old_mut().name[..n.len()].copy_from_slice(n.as_bytes()); // raw: set_path drops "./"
            h.set_cksum();
            b.append(&h, d).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        assert!(list(&out, None).unwrap().iter().any(|e| e.name.starts_with("./")));
        assert_eq!(read_entry(&out, "d/b.txt", None, 100, &Progress::none()).unwrap().bytes, b"beta");
        let dest = t.path().join("x");
        extract(&out, &dest, None, Some(&["a.txt".to_string()]), &Progress::none()).unwrap();
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
        assert!(!dest.join("d").exists());
    }

    #[test]
    fn parallel_zip_same_path_entries_do_not_interleave() {
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("case.zip");
        let mut w = zip::ZipWriter::new(File::create(&out).unwrap());
        let o: zip::write::SimpleFileOptions = Default::default();
        let (big_a, big_b) = (vec![b'A'; 3 << 20], vec![b'b'; 3 << 20]);
        w.start_file("README.txt", o).unwrap();
        w.write_all(&big_a).unwrap();
        w.start_file("other.bin", o).unwrap();
        w.write_all(&vec![7u8; 5 << 20]).unwrap();
        w.start_file("readme.txt", o).unwrap();
        w.write_all(&big_b).unwrap();
        w.finish().unwrap();
        let dest = t.path().join("x");
        extract_zip(&out, &dest, None, None, 4, &Progress::none()).unwrap();
        let got = fs::read(dest.join("readme.txt")).unwrap();
        assert!(got == big_a || got == big_b, "interleaved content");
    }

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
    fn crc_and_test_and_delete() {
        for fmt in [Format::Zip, Format::SevenZ, Format::TarGz, Format::TarZst] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            let out = t.path().join(format!("a.{}", fmt.extension()));
            create(&out, &[src], &CreateOptions { format: fmt, level: None, password: None }, &Progress::none()).unwrap();
            let l = list(&out, None).unwrap();
            let hello = l.iter().find(|e| e.name.ends_with("hello.txt")).unwrap();
            if matches!(fmt, Format::Zip | Format::SevenZ) {
                assert_eq!(hello.crc32, Some(crc32fast::hash(&b"hello world\n".repeat(100))), "{fmt:?}");
            }
            assert_eq!(test(&out, None, &Progress::none()).unwrap(), 4, "{fmt:?}");
            delete(&out, &["data/sub".to_string()], None, &Progress::none()).unwrap();
            let names: Vec<String> = list(&out, None).unwrap().into_iter().map(|e| e.name).collect();
            assert!(names.iter().any(|n| n.ends_with("hello.txt")), "{fmt:?} {names:?}");
            assert!(!names.iter().any(|n| n.contains("sub/")), "{fmt:?} {names:?}");
            assert!(!t.path().join(format!("a.{}.rewrite.tmp", fmt.extension())).exists());
        }
    }

    #[test]
    fn test_detects_corruption() {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join("c.zip");
        create(&out, &[src], &CreateOptions { format: Format::Zip, level: Some(0), password: None }, &Progress::none()).unwrap();
        let mut b = fs::read(&out).unwrap();
        let pos = b.windows(11).position(|w| w == b"hello world").unwrap();
        b[pos] ^= 0xFF;
        fs::write(&out, b).unwrap();
        assert!(test(&out, None, &Progress::none()).is_err());
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

    fn read(a: &Path, name: &str, pw: Option<&str>, max: usize) -> Result<EntryData> {
        read_entry(a, name, pw, max, &Progress::none())
    }

    fn read_entry_cases(fmt: Format, pw: Option<&str>) {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join(format!("r.{}", fmt.extension()));
        create(&out, &[src.clone()], &CreateOptions { format: fmt, level: None, password: pw.map(String::from) }, &Progress::none()).unwrap();
        let hello = fs::read(src.join("hello.txt")).unwrap();
        assert_eq!(read(&out, "data/hello.txt", pw, 1 << 20).unwrap(), EntryData { bytes: hello.clone(), truncated: false }, "{fmt:?}");
        assert_eq!(read(&out, "data/sub/deeper/中文.txt", pw, 100).unwrap().bytes, "測試內容".as_bytes(), "{fmt:?}");
        let bin = fs::read(src.join("sub/a.bin")).unwrap();
        let r = read(&out, "data/sub/a.bin", pw, 1000).unwrap();
        assert!(r.truncated && r.bytes == bin[..1000], "{fmt:?}");
        assert_eq!(read(&out, "data/sub/a.bin", pw, bin.len()).unwrap(), EntryData { bytes: bin.clone(), truncated: false }, "{fmt:?}");
        assert_eq!(read(&out, "data/hello.txt", pw, 0).unwrap(), EntryData { bytes: vec![], truncated: true }, "{fmt:?}");
        assert_eq!(read(&out, "data/sub/empty.txt", pw, 0).unwrap(), EntryData { bytes: vec![], truncated: false }, "{fmt:?}");
        // every listed file entry is readable by its listed name; directories are not
        for e in list(&out, pw).unwrap() {
            let r = read(&out, &e.name, pw, usize::MAX);
            if e.is_dir {
                assert!(matches!(r, Err(Error::NotFound(_))), "{fmt:?} {}: {r:?}", e.name);
            } else {
                assert_eq!(r.unwrap().bytes.len() as u64, e.size, "{fmt:?} {}", e.name);
            }
        }
        for missing in ["data/nope.txt", "data/sub", "data/sub/", "data", "hello.txt"] {
            assert!(matches!(read(&out, missing, pw, 10), Err(Error::NotFound(_))), "{fmt:?} {missing}");
        }
        let p = Progress { on: &|_, _, _| {}, cancel: Arc::new(AtomicBool::new(true)) };
        assert!(matches!(read_entry(&out, "data/hello.txt", pw, 10, &p), Err(Error::Cancelled)), "{fmt:?}");
    }

    #[test] fn read_entry_zip() { read_entry_cases(Format::Zip, None) }
    #[test] fn read_entry_zip_password() { read_entry_cases(Format::Zip, Some("s3cret")) }
    #[test] fn read_entry_7z() { read_entry_cases(Format::SevenZ, None) }
    #[test] fn read_entry_7z_password() { read_entry_cases(Format::SevenZ, Some("s3cret")) }
    #[test] fn read_entry_tar() { read_entry_cases(Format::Tar, None) }
    #[test] fn read_entry_tar_gz() { read_entry_cases(Format::TarGz, None) }
    #[test] fn read_entry_tar_zst() { read_entry_cases(Format::TarZst, None) }

    #[test]
    fn read_entry_wrong_password() {
        for fmt in [Format::Zip, Format::SevenZ] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            let out = t.path().join(format!("w.{}", fmt.extension()));
            create(&out, &[src], &CreateOptions { format: fmt, level: None, password: Some("right".into()) }, &Progress::none()).unwrap();
            assert!(read(&out, "data/hello.txt", Some("wrong"), 100).is_err(), "{fmt:?}");
            assert!(read(&out, "data/hello.txt", None, 100).is_err(), "{fmt:?}");
        }
    }

    #[test]
    fn read_entry_7z_multi_block() {
        // non-solid archive: one block per file, only the entry's block is decoded
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("m.7z");
        let mut w = sevenz_rust2::ArchiveWriter::create(&out).unwrap();
        let parts: Vec<(String, Vec<u8>)> = (0..4).map(|k| (format!("f{k}.txt"), format!("block {k} ").repeat(1000 * (k + 1)).into_bytes())).collect();
        for (name, data) in &parts {
            w.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file(name), Some(data.as_slice())).unwrap();
        }
        w.finish().unwrap();
        assert_eq!(sevenz_rust2::Archive::open(&out).unwrap().blocks.len(), 4);
        for (name, data) in parts.iter().rev() {
            assert_eq!(&read(&out, name, None, usize::MAX).unwrap().bytes, data);
            let r = read(&out, name, None, 7).unwrap();
            assert!(r.truncated && r.bytes == data[..7]);
        }
    }

    #[test]
    fn read_entry_single_files() {
        for fmt in [Format::Gz, Format::Bz2, Format::Xz, Format::Zst] {
            let t = tempfile::tempdir().unwrap();
            let f = t.path().join("note.txt");
            let data = b"single file payload ".repeat(500);
            fs::write(&f, &data).unwrap();
            let out = t.path().join(format!("note.txt.{}", fmt.extension()));
            create(&out, &[f], &CreateOptions { format: fmt, level: None, password: None }, &Progress::none()).unwrap();
            assert_eq!(read(&out, "note.txt", None, data.len()).unwrap(), EntryData { bytes: data.clone(), truncated: false }, "{fmt:?}");
            let r = read(&out, "note.txt", None, 33).unwrap();
            assert!(r.truncated && r.bytes == data[..33], "{fmt:?}");
            assert_eq!(read(&out, "note.txt", None, 0).unwrap(), EntryData { bytes: vec![], truncated: true }, "{fmt:?}");
            assert!(matches!(read(&out, "other.txt", None, 10), Err(Error::NotFound(_))), "{fmt:?}");
        }
    }

    /// Many files of mixed sizes (0 B .. ~1.5 MiB) in nested folders.
    fn mixed_tree(dir: &Path) -> PathBuf {
        let root = dir.join("mix");
        for k in 0..60usize {
            let d = root.join(format!("d{}/e{}", k % 5, k % 3));
            fs::create_dir_all(&d).unwrap();
            let len = match k % 6 { 0 => 0, 1 => 17, 2 => 4096, 3 => 70_000, 4 => 300_000, _ => 1_500_000 } + k;
            let data: Vec<u8> = (0..len).map(|i| ((i * 7 + k * 13) % 251) as u8 ^ (i >> 9) as u8).collect();
            fs::write(d.join(format!("f{k}.bin")), data).unwrap();
        }
        fs::create_dir_all(root.join("empty/inner")).unwrap();
        root
    }

    #[test]
    fn zip_parallel_extraction() {
        let t = tempfile::tempdir().unwrap();
        let src = mixed_tree(t.path());
        let out = t.path().join("m.zip");
        create(&out, &[src.clone()], &CreateOptions { format: Format::Zip, level: Some(1), password: None }, &Progress::none()).unwrap();
        for th in [1, 2, 4, 8] {
            let dest = t.path().join(format!("all{th}"));
            fs::create_dir_all(&dest).unwrap();
            extract_zip(&out, &dest, None, None, th, &Progress::none()).unwrap();
            assert_same(&src, &dest.join("mix"));
            // subset: one folder plus one single file
            let dest = t.path().join(format!("sub{th}"));
            fs::create_dir_all(&dest).unwrap();
            let sel = ["mix/d1".to_string(), "mix/d2/e2/f2.bin".to_string()];
            extract_zip(&out, &dest, None, Some(&sel), th, &Progress::none()).unwrap();
            assert_same(&src.join("d1"), &dest.join("mix/d1"));
            assert_eq!(fs::read(dest.join("mix/d2/e2/f2.bin")).unwrap(), fs::read(src.join("d2/e2/f2.bin")).unwrap());
            assert!(!dest.join("mix/d0").exists() && !dest.join("mix/d2/e0").exists() && !dest.join("mix/empty").exists());
            // progress reaches the total once all bytes are written
            let last = std::sync::Mutex::new((0, 0));
            let on = |d, tot, _: &str| *last.lock().unwrap() = (d, tot);
            let p = Progress { on: &on, cancel: Arc::new(AtomicBool::new(false)) };
            let dest = t.path().join(format!("p{th}"));
            fs::create_dir_all(&dest).unwrap();
            extract_zip(&out, &dest, None, None, th, &p).unwrap();
            let (d, tot) = *last.lock().unwrap();
            assert!(d == tot && tot > 0, "{th}: {d}/{tot}");
            // cancellation
            let p = Progress { on: &|_, _, _| {}, cancel: Arc::new(AtomicBool::new(true)) };
            assert!(matches!(extract_zip(&out, &dest, None, None, th, &p), Err(Error::Cancelled)));
        }
    }

    #[test]
    fn zip_parallel_wrong_password() {
        let t = tempfile::tempdir().unwrap();
        let src = mixed_tree(t.path());
        let out = t.path().join("m.zip");
        create(&out, &[src.clone()], &CreateOptions { format: Format::Zip, level: Some(1), password: Some("right".into()) }, &Progress::none()).unwrap();
        for d in ["x", "y"] { fs::create_dir_all(t.path().join(d)).unwrap(); }
        assert!(extract_zip(&out, &t.path().join("x"), Some("wrong"), None, 4, &Progress::none()).is_err());
        extract_zip(&out, &t.path().join("y"), Some("right"), None, 4, &Progress::none()).unwrap();
        assert_same(&src, &t.path().join("y/mix"));
    }

    #[test]
    fn zip_partial_skips_unselected_encrypted() {
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("mixed.zip");
        let mut z = zip::ZipWriter::new(File::create(&out).unwrap());
        let opt = zip::write::SimpleFileOptions::default();
        z.start_file("a.txt", opt.with_aes_encryption(zip::AesMode::Aes256, "pw1")).unwrap();
        z.write_all(b"alpha").unwrap();
        z.start_file("b.txt", opt.with_aes_encryption(zip::AesMode::Aes256, "other")).unwrap();
        z.write_all(b"bravo").unwrap();
        z.start_file("c.txt", opt).unwrap();
        z.write_all(b"charlie").unwrap();
        z.finish().unwrap();
        let sel = ["a.txt".to_string(), "c.txt".to_string()];
        for th in [1, 4] {
            let dest = t.path().join(format!("x{th}"));
            fs::create_dir_all(&dest).unwrap();
            extract_zip(&out, &dest, Some("pw1"), Some(&sel), th, &Progress::none()).unwrap();
            assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
            assert_eq!(fs::read(dest.join("c.txt")).unwrap(), b"charlie");
            assert!(!dest.join("b.txt").exists());
            let all = t.path().join(format!("all{th}"));
            fs::create_dir_all(&all).unwrap();
            assert!(extract_zip(&out, &all, Some("pw1"), None, th, &Progress::none()).is_err());
        }
        extract(&out, &t.path().join("e"), Some("pw1"), Some(&sel), &Progress::none()).unwrap();
        assert_eq!(read(&out, "b.txt", Some("other"), 100).unwrap().bytes, b"bravo");
        assert_eq!(read(&out, "c.txt", None, 100).unwrap().bytes, b"charlie");
    }

    #[test]
    fn mtime_preserved() {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86_400 + 1234);
        for fmt in [Format::Zip, Format::SevenZ, Format::TarGz] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            for f in ["hello.txt", "sub/a.bin", "sub/empty.txt"] {
                File::options().write(true).open(src.join(f)).unwrap().set_modified(when).unwrap();
            }
            let out = t.path().join(format!("t.{}", fmt.extension()));
            create(&out, &[src], &CreateOptions { format: fmt, level: None, password: None }, &Progress::none()).unwrap();
            let dest = t.path().join("x");
            extract(&out, &dest, None, None, &Progress::none()).unwrap();
            for f in ["hello.txt", "sub/a.bin", "sub/empty.txt"] {
                let m = fs::metadata(dest.join("data").join(f)).unwrap().modified().unwrap();
                let diff = m.duration_since(when).unwrap_or_else(|e| e.duration());
                assert!(diff <= std::time::Duration::from_secs(2), "{fmt:?} {f}: off by {diff:?}");
            }
        }
    }

    #[test]
    fn sevenz_lists_encryption() {
        for pw in [None, Some("s3cret")] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            let out = t.path().join("e.7z");
            create(&out, &[src], &CreateOptions { format: Format::SevenZ, level: None, password: pw.map(String::from) }, &Progress::none()).unwrap();
            let l = list(&out, pw).unwrap();
            let hello = l.iter().find(|e| e.name.ends_with("hello.txt")).unwrap();
            assert_eq!(hello.encrypted, pw.is_some());
            assert!(l.iter().filter(|e| e.is_dir).all(|e| !e.encrypted));
        }
    }

    #[test]
    fn safe_join_blocks_traversal() {
        assert!(safe_join(Path::new("/d"), "../etc/passwd").is_none());
        assert!(safe_join(Path::new("/d"), "/abs").is_none());
        assert_eq!(safe_join(Path::new("/d"), "a/b").unwrap(), Path::new("/d/a/b"));
    }
}
