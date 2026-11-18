//! Multi-selection aggregation for Explorer context-menu verbs.
//! Explorer starts one process per selected file; the first process ("leader")
//! gathers the paths of all the others so the user gets one window / one batch.
//! CONTRACT STUB (workstream B implements; signature is fixed).

use std::path::PathBuf;

/// `Some(all_paths)` in the leader process (including `mine`), `None` in a follower
/// (whose paths were handed to the leader; the caller should exit 0 without UI).
pub fn gather(verb: &str, mine: &[PathBuf]) -> Option<Vec<PathBuf>> {
    let _ = verb;
    Some(mine.to_vec())
}
