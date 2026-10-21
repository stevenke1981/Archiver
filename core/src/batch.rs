//! Batch operations: compress every input into its own archive, extract many archives in one
//! go. Items are processed one after another; a failing item does not stop the batch (only
//! cancellation does), and progress is reported for the batch as a whole.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::{CreateOptions, Error, Format, Progress, Result};

const U: u64 = 1 << 20;

/// Where [`extract_each`] puts the contents of each archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchDest {
    /// Into the folder that contains the archive.
    Beside,
    /// Into `<folder of the archive>/<archive stem>/`.
    BesideInFolder,
    /// Into `<dir>/<archive stem>/`.
    IntoFolders(PathBuf),
}

/// Outcome of one batch item.
#[derive(Debug)]
pub struct BatchResult {
    /// The input file/folder (create) or archive (extract).
    pub input: PathBuf,
    /// The archive written (create) or the destination folder (extract).
    pub output: PathBuf,
    pub result: Result<()>,
}

/// Destination folder of `archive` for `dest`.
pub fn batch_dest(archive: &Path, dest: &BatchDest) -> PathBuf {
    let parent = archive.parent().unwrap_or(Path::new(""));
    match dest {
        BatchDest::Beside => parent.to_path_buf(),
        BatchDest::BesideInFolder => parent.join(super::archive_stem(archive)),
        BatchDest::IntoFolders(d) => d.join(super::archive_stem(archive)),
    }
}

/// Output archive path for `input` inside `out_dir` (default: the input's own folder). Never
/// returns a path that exists or is in `taken` (adds " (2)", " (3)", … before the extension).
pub fn batch_output(input: &Path, out_dir: Option<&Path>, fmt: Format, taken: &HashSet<PathBuf>) -> PathBuf {
    let file = super::suggest_output(&[input.to_path_buf()], fmt);
    let name = file.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = format!(".{}", fmt.extension());
    let stem = name.strip_suffix(ext.as_str()).unwrap_or(name.as_str()).to_string();
    let dir = out_dir.unwrap_or(input.parent().unwrap_or(Path::new(""))).to_path_buf();
    let mut out = dir.join(&name);
    let mut n = 2;
    while out.exists() || taken.contains(&out) {
        out = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    out
}

/// Compress each of `inputs` into its own archive (see [`batch_output`]).
pub fn create_each(inputs: &[PathBuf], out_dir: Option<&Path>, opts: &CreateOptions, p: &Progress) -> Vec<BatchResult> {
    let n = inputs.len();
    let dir_err = out_dir.and_then(|d| fs::create_dir_all(d).err());
    let mut taken = HashSet::new();
    let mut stop = false;
    inputs
        .iter()
        .enumerate()
        .map(|(k, input)| {
            let output = batch_output(input, out_dir, opts.format, &taken);
            taken.insert(output.clone());
            let result = if let Some(e) = &dir_err {
                Err(io_err(e))
            } else if stop || cancelled(p) {
                Err(Error::Cancelled)
            } else {
                item(p, k, n, &name_of(input), |ip| super::create(&output, std::slice::from_ref(input), opts, ip))
            };
            stop |= matches!(result, Err(Error::Cancelled));
            BatchResult { input: input.clone(), output, result }
        })
        .collect()
}

/// Extract each of `archives` (see [`batch_dest`]). Archives that would share a per-archive
/// folder within the batch (`a.zip` + `a.7z`, `x/data.zip` + `y/data.zip` into one folder) get
/// "<stem> (2)", "<stem> (3)", … instead of overwriting each other.
pub fn extract_each(archives: &[PathBuf], dest: &BatchDest, password: Option<&str>, p: &Progress) -> Vec<BatchResult> {
    let n = archives.len();
    let mut stop = false;
    let mut taken = HashSet::new();
    archives
        .iter()
        .enumerate()
        .map(|(k, archive)| {
            let mut output = batch_dest(archive, dest);
            if *dest != BatchDest::Beside {
                let base = output.clone();
                let key = |p: &Path| p.to_string_lossy().to_lowercase();
                let mut i = 2;
                while taken.contains(&key(&output)) {
                    let name = base.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    output = base.with_file_name(format!("{name} ({i})"));
                    i += 1;
                }
                taken.insert(key(&output));
            }
            let result = if stop || cancelled(p) {
                Err(Error::Cancelled)
            } else {
                item(p, k, n, &name_of(archive), |ip| super::extract(archive, &output, password, None, ip))
            };
            stop |= matches!(result, Err(Error::Cancelled));
            BatchResult { input: archive.clone(), output, result }
        })
        .collect()
}

/// Runs item `k` of `n` with a progress callback mapped onto the batch's `n * U` scale.
fn item(p: &Progress, k: usize, n: usize, name: &str, f: impl FnOnce(&Progress) -> Result<()>) -> Result<()> {
    let total = (n as u64).saturating_mul(U);
    let base = (k as u64).saturating_mul(U);
    let label = format!("[{}/{}] {name}", k + 1, n);
    let on = |d: u64, t: u64, _: &str| {
        let frac = if t == 0 { 0 } else { (d.min(t) as u128 * U as u128 / t as u128) as u64 };
        (p.on)(base.saturating_add(frac), total, &label)
    };
    let r = f(&Progress { on: &on, cancel: p.cancel.clone() });
    (p.on)(base.saturating_add(U), total, &label);
    r
}

fn cancelled(p: &Progress) -> bool {
    p.cancel.load(Ordering::Relaxed)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

fn io_err(e: &io::Error) -> Error {
    Error::Io(io::Error::new(e.kind(), e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    #[test]
    fn extract_folders_unique_within_batch() {
        let t = tempfile::tempdir().unwrap();
        let (x, y) = (t.path().join("x"), t.path().join("y"));
        let mut arcs = Vec::new();
        for (d, fmt, body) in [(&x, Format::Zip, "one"), (&y, Format::Zip, "two"), (&x, Format::SevenZ, "three")] {
            fs::create_dir_all(d).unwrap();
            let f = d.join("data.txt");
            fs::write(&f, body).unwrap();
            let out = d.join(format!("data.{}", fmt.extension()));
            super::super::create(&out, &[f.clone()], &CreateOptions { format: fmt, level: None, password: None }, &Progress::none()).unwrap();
            fs::remove_file(&f).unwrap();
            arcs.push(out);
        }
        let ex = t.path().join("ex");
        let r = extract_each(&arcs, &BatchDest::IntoFolders(ex.clone()), None, &Progress::none());
        assert!(r.iter().all(|r| r.result.is_ok()));
        let outs: Vec<_> = r.iter().map(|r| r.output.clone()).collect();
        assert_eq!(outs, vec![ex.join("data"), ex.join("data (2)"), ex.join("data (3)")]);
        assert_eq!(fs::read_to_string(ex.join("data (2)/data.txt")).unwrap(), "two");
        // BesideInFolder: a.zip and a.7z next to each other
        let r = extract_each(&[arcs[0].clone(), arcs[2].clone()], &BatchDest::BesideInFolder, None, &Progress::none());
        assert_eq!(r[1].output, x.join("data (2)"));
        assert_eq!(fs::read_to_string(x.join("data (2)/data.txt")).unwrap(), "three");
    }

    fn src(t: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let s = t.join("src");
        fs::create_dir_all(s.join("Photos/sub")).unwrap();
        fs::write(s.join("a.txt"), b"alpha alpha\n".repeat(50)).unwrap();
        fs::write(s.join("b.bin"), (0..20_000u32).map(|i| (i % 253) as u8).collect::<Vec<_>>()).unwrap();
        fs::write(s.join("Photos/x.txt"), b"photo").unwrap();
        fs::write(s.join("Photos/sub/y.txt"), "中文內容".as_bytes()).unwrap();
        (s.join("a.txt"), s.join("b.bin"), s.join("Photos"))
    }

    fn opts(format: Format) -> CreateOptions {
        CreateOptions { format, level: None, password: None }
    }

    #[test]
    fn create_then_extract_roundtrip() {
        for format in [Format::Zip, Format::TarGz] {
            let t = tempfile::tempdir().unwrap();
            let (a, b, photos) = src(t.path());
            let out = t.path().join("out");
            let rs = create_each(&[a.clone(), b.clone(), photos.clone()], Some(&out), &opts(format), &Progress::none());
            assert_eq!(rs.len(), 3);
            assert!(rs.iter().all(|r| r.result.is_ok()), "{format:?}: {rs:?}");
            let ext = format.extension();
            assert_eq!(rs[0].output, out.join(format!("a.txt.{ext}")));
            assert_eq!(rs[2].output, out.join(format!("Photos.{ext}")));

            let outputs: Vec<PathBuf> = rs.iter().map(|r| r.output.clone()).collect();
            let ex = t.path().join("ex");
            let rs = extract_each(&outputs, &BatchDest::IntoFolders(ex.clone()), None, &Progress::none());
            assert!(rs.iter().all(|r| r.result.is_ok()), "{format:?}: {rs:?}");
            assert_eq!(fs::read(ex.join("a.txt/a.txt")).unwrap(), fs::read(&a).unwrap());
            assert_eq!(fs::read(ex.join("b.bin/b.bin")).unwrap(), fs::read(&b).unwrap());
            assert_eq!(fs::read(ex.join("Photos/Photos/x.txt")).unwrap(), b"photo");
            assert_eq!(fs::read(ex.join("Photos/Photos/sub/y.txt")).unwrap(), "中文內容".as_bytes());
        }
    }

    #[test]
    fn output_name_collisions() {
        let t = tempfile::tempdir().unwrap();
        let (a, _, photos) = src(t.path());
        let out = t.path().join("out");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("a.txt.zip"), b"old").unwrap();
        fs::write(out.join("Photos.tar.gz"), b"old").unwrap();
        assert_eq!(batch_output(&a, Some(&out), Format::Zip, &HashSet::new()), out.join("a.txt (2).zip"));
        assert_eq!(batch_output(&photos, Some(&out), Format::TarGz, &HashSet::new()), out.join("Photos (2).tar.gz"));
        let taken: HashSet<PathBuf> = [out.join("a.txt (2).zip")].into_iter().collect();
        assert_eq!(batch_output(&a, Some(&out), Format::Zip, &taken), out.join("a.txt (3).zip"));

        // same file name from two folders into one out dir
        let s2 = t.path().join("other");
        fs::create_dir_all(&s2).unwrap();
        let a2 = s2.join("a.txt");
        fs::write(&a2, b"second").unwrap();
        let fresh = t.path().join("fresh");
        let rs = create_each(&[a.clone(), a2.clone()], Some(&fresh), &opts(Format::Zip), &Progress::none());
        assert!(rs.iter().all(|r| r.result.is_ok()), "{rs:?}");
        assert_eq!(rs[0].output, fresh.join("a.txt.zip"));
        assert_eq!(rs[1].output, fresh.join("a.txt (2).zip"));
        assert!(rs[0].output.exists() && rs[1].output.exists());
    }

    #[test]
    fn single_file_format_fails_only_for_folder() {
        let t = tempfile::tempdir().unwrap();
        let (a, _, photos) = src(t.path());
        let out = t.path().join("out");
        let rs = create_each(&[a, photos], Some(&out), &opts(Format::Gz), &Progress::none());
        assert!(rs[0].result.is_ok());
        assert!(rs[1].result.is_err());
        assert_eq!(rs[0].output, out.join("a.txt.gz"));
        assert!(out.join("a.txt.gz").exists());
        assert!(!rs[1].output.exists());
    }

    #[test]
    fn extract_continues_after_corrupt_archive() {
        let t = tempfile::tempdir().unwrap();
        let (a, _, _) = src(t.path());
        let dir = t.path().join("arc");
        let rs = create_each(&[a], Some(&dir), &opts(Format::Zip), &Progress::none());
        let good = rs[0].output.clone();
        let bad = dir.join("bad.zip");
        fs::write(&bad, b"garbage bytes, not a zip").unwrap();
        let good2 = dir.join("good2.zip");
        fs::copy(&good, &good2).unwrap();

        let archives = vec![good.clone(), bad.clone(), good2.clone()];
        let rs = extract_each(&archives, &BatchDest::BesideInFolder, None, &Progress::none());
        assert!(rs[0].result.is_ok());
        assert!(rs[1].result.is_err());
        assert!(rs[2].result.is_ok());
        assert_eq!(rs[0].output, dir.join("a.txt"));
        assert_eq!(rs[2].output, dir.join("good2"));
        assert_eq!(fs::read(dir.join("a.txt/a.txt")).unwrap().len(), 600);
        assert_eq!(fs::read(dir.join("good2/a.txt")).unwrap().len(), 600);

        let ex = t.path().join("ex");
        let rs = extract_each(&archives, &BatchDest::IntoFolders(ex.clone()), None, &Progress::none());
        assert!(rs[0].result.is_ok() && rs[1].result.is_err() && rs[2].result.is_ok());
        assert_eq!(rs[1].output, ex.join("bad"));
        assert!(ex.join("a.txt/a.txt").exists());
        assert!(ex.join("good2/a.txt").exists());
    }

    #[test]
    fn cancelled_before_start_gives_cancelled_for_all() {
        let t = tempfile::tempdir().unwrap();
        let (a, b, photos) = src(t.path());
        let out = t.path().join("out");
        let on = |_: u64, _: u64, _: &str| {};
        let p = Progress { on: &on, cancel: Arc::new(AtomicBool::new(true)) };
        let inputs = vec![a, b, photos];
        let rs = create_each(&inputs, Some(&out), &opts(Format::Zip), &p);
        assert_eq!(rs.len(), 3);
        for (r, i) in rs.iter().zip(&inputs) {
            assert_eq!(&r.input, i);
            assert!(matches!(r.result, Err(Error::Cancelled)));
        }
        assert!(!out.join("a.txt.zip").exists());

        let rs = extract_each(&[out.join("x.zip")], &BatchDest::Beside, None, &p);
        assert!(matches!(rs[0].result, Err(Error::Cancelled)));
    }

    #[test]
    fn cancel_during_batch_stops_remaining_items() {
        let t = tempfile::tempdir().unwrap();
        let (a, b, photos) = src(t.path());
        let out = t.path().join("out");
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        // cancel while item 2 (index 1) is running
        let on = move |_: u64, _: u64, name: &str| {
            if name.starts_with("[2/3]") {
                c2.store(true, Ordering::Relaxed);
            }
        };
        let p = Progress { on: &on, cancel };
        let rs = create_each(&[a, b, photos], Some(&out), &opts(Format::Zip), &p);
        assert_eq!(rs.len(), 3);
        assert!(rs[0].result.is_ok());
        assert!(matches!(rs[2].result, Err(Error::Cancelled)));
        assert!(!rs[2].output.exists());
    }

    #[test]
    fn progress_is_monotonic_within_total() {
        let t = tempfile::tempdir().unwrap();
        let (a, b, photos) = src(t.path());
        let out = t.path().join("out");
        let log = Mutex::new(Vec::<(u64, u64, String)>::new());
        let on = |d: u64, tot: u64, name: &str| log.lock().unwrap().push((d, tot, name.to_string()));
        let p = Progress { on: &on, cancel: Arc::new(AtomicBool::new(false)) };
        let rs = create_each(&[a, b, photos], Some(&out), &opts(Format::TarGz), &p);
        assert!(rs.iter().all(|r| r.result.is_ok()), "{rs:?}");
        let log = log.into_inner().unwrap();
        assert!(!log.is_empty());
        let total = 3 * U;
        for (d, tot, _) in &log {
            assert_eq!(*tot, total);
            assert!(*d <= total);
        }
        assert_eq!(log.last().map(|l| (l.0, l.1)), Some((total, total)));
        assert!(log[0].2.starts_with("[1/3]"));
        assert!(log.iter().any(|(_, _, n)| n.starts_with("[3/3]")));

        let log = Mutex::new(Vec::<(u64, u64)>::new());
        let on = |d: u64, tot: u64, _: &str| log.lock().unwrap().push((d, tot));
        let p = Progress { on: &on, cancel: Arc::new(AtomicBool::new(false)) };
        let outputs: Vec<PathBuf> = rs.iter().map(|r| r.output.clone()).collect();
        let rs = extract_each(&outputs, &BatchDest::IntoFolders(t.path().join("ex")), None, &p);
        assert!(rs.iter().all(|r| r.result.is_ok()), "{rs:?}");
        let log = log.into_inner().unwrap();
        assert!(log.iter().all(|&(d, tot)| tot == total && d <= total));
        assert_eq!(log.last().copied(), Some((total, total)));
    }

    #[test]
    fn out_dir_failure_marks_every_item() {
        let t = tempfile::tempdir().unwrap();
        let (a, b, _) = src(t.path());
        let blocker = t.path().join("blocker");
        fs::write(&blocker, b"file").unwrap();
        let rs = create_each(&[a, b], Some(&blocker.join("sub")), &opts(Format::Zip), &Progress::none());
        assert_eq!(rs.len(), 2);
        assert!(rs.iter().all(|r| matches!(r.result, Err(Error::Io(_)))));
    }

    #[test]
    fn batch_dest_variants() {
        let arc = Path::new("/x/My.Backup.tar.gz");
        assert_eq!(batch_dest(arc, &BatchDest::Beside), PathBuf::from("/x"));
        assert_eq!(batch_dest(arc, &BatchDest::BesideInFolder), PathBuf::from("/x/My.Backup"));
        assert_eq!(batch_dest(arc, &BatchDest::IntoFolders(PathBuf::from("/out"))), PathBuf::from("/out/My.Backup"));
    }
}
