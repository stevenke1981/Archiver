//! Multi-selection aggregation for Explorer context-menu verbs.
//! Explorer starts one process per selected file; the first process ("leader")
//! gathers the paths of all the others so the user gets one window / one batch.
//!
//! Protocol (std only), spool dir `<base>/spool/<verb>/`:
//! 1. every process writes its paths to a new spool file `<nanos>-<pid>-<n>` (atomic: tmp + rename);
//! 2. it then tries `create_new` on `leader.lock`; on failure it is a follower and exits (its file is
//!    swept by the leader: the leader releases the lock *before* its final sweep);
//! 3. the leader first claims its own file (atomic rename; if somebody else already claimed it the
//!    paths are theirs and this process is a follower), then sweeps until no new file appeared for
//!    `quiet` (capped by `cap`), releases the lock, and sweeps once more.
//!
//! Every file is claimed by exactly one sweep (rename to a unique name), so no path is lost or doubled.
//! A lock older than `stale_lock` (crashed leader) is stolen.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Timings and location; `Config::default()` is what the real process uses.
#[derive(Debug, Clone)]
pub struct Config {
    /// Parent of the `spool` directory.
    pub base: PathBuf,
    /// The leader finishes when no new file arrived for this long.
    pub quiet: Duration,
    /// Upper bound for the leader's wait.
    pub cap: Duration,
    /// A `leader.lock` older than this is considered abandoned.
    pub stale_lock: Duration,
    /// Poll interval of the leader.
    pub poll: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            base: default_base(),
            quiet: Duration::from_millis(250),
            cap: Duration::from_secs(3),
            stale_lock: Duration::from_secs(10),
            poll: Duration::from_millis(20),
        }
    }
}

/// `%LOCALAPPDATA%\Archiver` on Windows, a temp-dir subfolder elsewhere.
pub fn default_base() -> PathBuf {
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("LOCALAPPDATA").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("Archiver");
    }
    std::env::temp_dir().join("archiver")
}

/// `Some(all_paths)` in the leader process (including `mine`), `None` in a follower
/// (whose paths were handed to the leader; the caller should exit 0 without UI).
pub fn gather(verb: &str, mine: &[PathBuf]) -> Option<Vec<PathBuf>> {
    gather_with(&Config::default(), verb, mine)
}

pub fn gather_with(cfg: &Config, verb: &str, mine: &[PathBuf]) -> Option<Vec<PathBuf>> {
    match try_gather(cfg, verb, mine) {
        Ok(r) => r,
        // spool unusable (read-only profile...): act alone rather than lose the request
        Err(_) => Some(mine.to_vec()),
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn verb_dir(cfg: &Config, verb: &str) -> PathBuf {
    let safe: String = verb.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    cfg.base.join("spool").join(if safe.is_empty() { "_".into() } else { safe })
}

fn is_spool_file(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_digit) && !name.ends_with(".tmp") && !name.ends_with(".claim")
}

fn try_gather(cfg: &Config, verb: &str, mine: &[PathBuf]) -> std::io::Result<Option<Vec<PathBuf>>> {
    let dir = verb_dir(cfg, verb);
    fs::create_dir_all(&dir)?;
    let name = format!("{:040}-{}-{}", nanos(), std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
    let tmp = dir.join(format!("{name}.tmp"));
    {
        let mut f = fs::File::create(&tmp)?;
        for p in mine {
            writeln!(f, "{}", p.display())?;
        }
        f.flush()?;
    }
    let own = dir.join(&name);
    fs::rename(&tmp, &own)?;

    let lock = dir.join("leader.lock");
    if !acquire(&lock, cfg.stale_lock) {
        return Ok(None);
    }
    // claim our own file; if a previous leader's final sweep took it, our paths are already theirs
    if claim(&own, &name).is_none() {
        let _ = fs::remove_file(&lock);
        return Ok(None);
    }

    let mut out: Vec<PathBuf> = mine.to_vec();
    let (start, mut last_new) = (Instant::now(), Instant::now());
    loop {
        std::thread::sleep(cfg.poll);
        let got = sweep(&dir);
        if !got.is_empty() {
            last_new = Instant::now();
            out.extend(got);
        }
        if last_new.elapsed() >= cfg.quiet || start.elapsed() >= cfg.cap {
            break;
        }
    }
    // release first, then sweep: a follower that failed to lock has its file in place already
    let _ = fs::remove_file(&lock);
    out.extend(sweep(&dir));
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    Ok(Some(out))
}

/// Create the lock; steal it when it is stale.
fn acquire(lock: &Path, stale: Duration) -> bool {
    for _ in 0..2 {
        match fs::OpenOptions::new().write(true).create_new(true).open(lock) {
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let age = fs::metadata(lock).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
                if age.is_some_and(|a| a > stale) {
                    let _ = fs::remove_file(lock);
                    continue;
                }
                return false;
            }
            Err(_) => return false,
        }
    }
    false
}

/// Atomically take ownership of a spool file and read its paths.
fn claim(path: &Path, name: &str) -> Option<Vec<PathBuf>> {
    let claimed = path.with_file_name(format!("{name}.{}-{}.claim", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    fs::rename(path, &claimed).ok()?;
    let text = fs::read_to_string(&claimed).unwrap_or_default();
    let _ = fs::remove_file(&claimed);
    Some(text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
}

fn sweep(dir: &Path) -> Vec<PathBuf> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.file_name().into_string().ok()).filter(|n| is_spool_file(n)).collect())
        .unwrap_or_default();
    names.sort();
    names.iter().filter_map(|n| claim(&dir.join(n), n)).flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn cfg(base: &Path) -> Config {
        Config {
            base: base.to_path_buf(),
            quiet: Duration::from_millis(200),
            cap: Duration::from_secs(5),
            stale_lock: Duration::from_millis(400),
            poll: Duration::from_millis(5),
        }
    }

    fn run(cfg: &Arc<Config>, verb: &'static str, n: usize, stagger_ms: u64) -> Vec<Option<Vec<PathBuf>>> {
        let hs: Vec<_> = (0..n)
            .map(|i| {
                let cfg = cfg.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(stagger_ms * i as u64));
                    gather_with(&cfg, verb, &[PathBuf::from(format!("files/f{i}.zip"))])
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    }

    fn assert_one_leader(results: Vec<Option<Vec<PathBuf>>>, n: usize) {
        let leaders: Vec<_> = results.into_iter().flatten().collect();
        assert_eq!(leaders.len(), 1, "exactly one leader expected");
        let mut got: Vec<String> = leaders[0].iter().map(|p| p.display().to_string()).collect();
        got.sort();
        let mut want: Vec<String> = (0..n).map(|i| format!("files/f{i}.zip")).collect();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn single_process_returns_its_own_paths() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path());
        let r = gather_with(&c, "extract", &[PathBuf::from("a.zip"), PathBuf::from("b.zip")]).unwrap();
        assert_eq!(r, vec![PathBuf::from("a.zip"), PathBuf::from("b.zip")]);
        // spool is left clean and the lock released
        let left: Vec<_> = fs::read_dir(verb_dir(&c, "extract")).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn simultaneous_processes_one_leader_gets_all() {
        let t = tempfile::tempdir().unwrap();
        let c = Arc::new(cfg(t.path()));
        assert_one_leader(run(&c, "v1", 20, 0), 20);
    }

    #[test]
    fn staggered_processes_one_leader_gets_all() {
        let t = tempfile::tempdir().unwrap();
        let c = Arc::new(cfg(t.path()));
        // 15 processes 8 ms apart: each arrives inside the 200 ms quiet window of the previous one
        assert_one_leader(run(&c, "v2", 15, 8), 15);
    }

    #[test]
    fn verbs_are_independent_and_spool_is_reusable() {
        let t = tempfile::tempdir().unwrap();
        let c = Arc::new(cfg(t.path()));
        let (c1, c2) = (c.clone(), c.clone());
        let a = std::thread::spawn(move || run(&c1, "va", 6, 1));
        let b = std::thread::spawn(move || run(&c2, "vb", 6, 1));
        assert_one_leader(a.join().unwrap(), 6);
        assert_one_leader(b.join().unwrap(), 6);
        // a second selection later behaves the same
        assert_one_leader(run(&c, "va", 4, 0), 4);
    }

    #[test]
    fn stale_lock_is_stolen_fresh_lock_is_respected() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path());
        let dir = verb_dir(&c, "stale");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("leader.lock"), b"").unwrap();
        // fresh lock: we are a follower and our file stays in the spool for the (absent) leader
        assert_eq!(gather_with(&c, "stale", &[PathBuf::from("x.zip")]), None);
        assert_eq!(sweep(&dir), vec![PathBuf::from("x.zip")]);
        // after it goes stale a new process takes over
        std::thread::sleep(c.stale_lock + Duration::from_millis(100));
        assert_eq!(gather_with(&c, "stale", &[PathBuf::from("y.zip")]), Some(vec![PathBuf::from("y.zip")]));
    }

    #[test]
    fn unusable_spool_falls_back_to_acting_alone() {
        let t = tempfile::tempdir().unwrap();
        let blocker = t.path().join("blocker");
        fs::write(&blocker, b"file, not dir").unwrap();
        let c = cfg(&blocker); // base is a file: create_dir_all fails
        assert_eq!(gather_with(&c, "v", &[PathBuf::from("a")]), Some(vec![PathBuf::from("a")]));
    }

    #[test]
    fn verb_names_are_sanitised() {
        let c = cfg(Path::new("/b"));
        assert_eq!(verb_dir(&c, "../x/y"), Path::new("/b").join("spool").join("___x_y"));
    }
}
