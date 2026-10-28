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

mod archive_ops;
mod create;
use archive_ops::*;
use create::*;
pub use archive_ops::{delete, extract, list, test};
pub use create::{CreateOptions, create, suggest_output};

#[cfg(test)]
mod tests;
