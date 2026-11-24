//! End-to-end tests of the `rarc` binary: --json output and deterministic exit codes.
use std::path::Path;
use std::process::{Command, Output};

fn rarc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rarc")).args(args).output().unwrap()
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&o.stdout)))
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn sample(dir: &Path) -> std::path::PathBuf {
    let d = dir.join("data");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("a.txt"), "hello hello hello").unwrap();
    std::fs::write(d.join("sub/b.txt"), "bee").unwrap();
    d
}

#[test]
fn create_list_test_extract_json() {
    let t = tempfile::tempdir().unwrap();
    let d = sample(t.path());
    let z = t.path().join("a.zip");
    let o = rarc(&["c", &s(&z), &s(&d), "--json"]);
    assert_eq!(code(&o), 0);
    assert_eq!(json(&o)["ok"], true);

    let o = rarc(&["l", &s(&z), "--json"]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    assert_eq!(v["ok"], true);
    let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"data/a.txt"), "{names:?}");
    assert!(v["entries"][0].get("is_dir").is_some() && v["entries"][0].get("crc32").is_some());

    let o = rarc(&["t", &s(&z), "--json"]);
    assert_eq!(code(&o), 0);
    assert_eq!(json(&o)["tested"], 2);

    let out = t.path().join("out");
    let o = rarc(&["x", &s(&z), "-o", &s(&out), "--json"]);
    assert_eq!(code(&o), 0);
    assert_eq!(std::fs::read_to_string(out.join("data/sub/b.txt")).unwrap(), "bee");
}

#[test]
fn comment_and_add() {
    let t = tempfile::tempdir().unwrap();
    let d = sample(t.path());
    let z = t.path().join("a.zip");
    assert_eq!(code(&rarc(&["c", &s(&z), &s(&d)])), 0);
    let o = rarc(&["comment", "show", &s(&z), "--json"]);
    assert!(json(&o)["comment"].is_null());
    assert_eq!(code(&rarc(&["comment", "set", &s(&z), "-m", "備註 note"])), 0);
    let o = rarc(&["comment", "show", &s(&z), "--json"]);
    assert_eq!(json(&o)["comment"], "備註 note");
    assert_eq!(String::from_utf8_lossy(&rarc(&["comment", "show", &s(&z)]).stdout).trim(), "備註 note");

    let extra = t.path().join("extra.txt");
    std::fs::write(&extra, "more").unwrap();
    let o = rarc(&["a", &s(&z), &s(&extra), "--prefix", "data/sub", "--json"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stdout));
    let o = rarc(&["l", &s(&z), "--json"]);
    let v = json(&o);
    assert!(v["entries"].as_array().unwrap().iter().any(|e| e["name"] == "data/sub/extra.txt"));
    assert_eq!(json(&rarc(&["comment", "show", &s(&z), "--json"]))["comment"], "備註 note");

    // comment on a non-ZIP: failure exit 1 with JSON error
    let tg = t.path().join("a.tar.gz");
    assert_eq!(code(&rarc(&["c", &s(&tg), &s(&d)])), 0);
    let o = rarc(&["comment", "set", &s(&tg), "-m", "x", "--json"]);
    assert_eq!(code(&o), 1);
    let v = json(&o);
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "unsupported");
}

#[test]
fn exit_codes() {
    let t = tempfile::tempdir().unwrap();
    let d = sample(t.path());
    // usage error
    assert_eq!(code(&rarc(&["bogus"])), 2);
    assert_eq!(code(&rarc(&["x"])), 2);
    // missing archive: failure
    let o = rarc(&["l", &s(&t.path().join("none.zip")), "--json"]);
    assert_eq!(code(&o), 1);
    assert_eq!(json(&o)["ok"], false);
    // password required / wrong
    let z = t.path().join("p.zip");
    assert_eq!(code(&rarc(&["c", &s(&z), &s(&d), "-p", "pw"])), 0);
    let o = rarc(&["t", &s(&z), "--json"]);
    assert_eq!(code(&o), 3, "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(json(&o)["kind"], "password");
    assert_eq!(code(&rarc(&["t", &s(&z), "-p", "wrong"])), 3);
    assert_eq!(code(&rarc(&["t", &s(&z), "-p", "pw"])), 0);
    let seven = t.path().join("p.7z");
    assert_eq!(code(&rarc(&["c", &s(&seven), &s(&d), "-p", "pw"])), 0);
    assert_eq!(code(&rarc(&["x", &s(&seven), "-o", &s(&t.path().join("o7"))])), 3);
}

#[test]
fn batch_json_and_partial_failure_code() {
    let t = tempfile::tempdir().unwrap();
    let d = sample(t.path());
    let f = t.path().join("one.txt");
    std::fs::write(&f, "x").unwrap();
    let o = rarc(&["ce", "-f", "zip", &s(&d), &s(&f), "--json"]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    assert_eq!(v["total"], 2);
    assert_eq!(v["failed"], 0);

    // one good archive + one corrupt file: partial failure = 4
    let bad = t.path().join("bad.zip");
    std::fs::write(&bad, b"not a zip at all").unwrap();
    let good = t.path().join("data.zip");
    let o = rarc(&["xe", &s(&good), &s(&bad), "-o", &s(&t.path().join("ex")), "--json"]);
    assert_eq!(code(&o), 4, "{}", String::from_utf8_lossy(&o.stdout));
    let v = json(&o);
    assert_eq!(v["ok"], false);
    assert_eq!(v["failed"], 1);
    // everything fails: 1
    let o = rarc(&["xe", &s(&bad), "-o", &s(&t.path().join("ex2"))]);
    assert_eq!(code(&o), 1);
}

#[test]
fn extract_overwrite_modes() {
    let t = tempfile::tempdir().unwrap();
    let d = sample(t.path());
    let z = t.path().join("a.zip");
    assert_eq!(code(&rarc(&["c", &s(&z), &s(&d), "--json"])), 0);
    let out = t.path().join("out");
    std::fs::create_dir_all(out.join("data")).unwrap();
    std::fs::write(out.join("data/a.txt"), "mine").unwrap();

    let o = rarc(&["x", &s(&z), "-o", &s(&out), "--overwrite", "skip", "--json"]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    assert_eq!(v["skipped"], 1);
    assert_eq!(std::fs::read_to_string(out.join("data/a.txt")).unwrap(), "mine");
    assert_eq!(std::fs::read_to_string(out.join("data/sub/b.txt")).unwrap(), "bee");

    let o = rarc(&["x", &s(&z), "-o", &s(&out), "--overwrite", "rename", "--json"]);
    assert_eq!(code(&o), 0);
    assert_eq!(json(&o)["renamed"], 2);
    assert_eq!(std::fs::read_to_string(out.join("data/a (2).txt")).unwrap(), "hello hello hello");
    assert_eq!(std::fs::read_to_string(out.join("data/a.txt")).unwrap(), "mine");

    let o = rarc(&["x", &s(&z), "-o", &s(&out), "--json"]);
    assert_eq!(code(&o), 0);
    assert_eq!(std::fs::read_to_string(out.join("data/a.txt")).unwrap(), "hello hello hello");

    assert_eq!(code(&rarc(&["x", &s(&z), "--overwrite", "bogus"])), 2);
}

#[test]
fn rename_entry_json() {
    let t = tempfile::tempdir().unwrap();
    let d = sample(t.path());
    let z = t.path().join("a.zip");
    assert_eq!(code(&rarc(&["c", &s(&z), &s(&d), "--json"])), 0);
    let o = rarc(&["rn", &s(&z), "data/sub", "data/renamed", "--json"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stdout));
    let v = json(&o);
    assert_eq!(v["ok"], true);
    assert_eq!(v["to"], "data/renamed");
    let v = json(&rarc(&["l", &s(&z), "--json"]));
    let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"data/renamed/b.txt"), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("data/sub")), "{names:?}");

    let o = rarc(&["rn", &s(&z), "nope.txt", "x.txt", "--json"]);
    assert_eq!(code(&o), 1);
    assert_eq!(json(&o)["kind"], "not_found");
}

#[test]
fn convert_json() {
    let t = tempfile::tempdir().unwrap();
    let d = sample(t.path());
    let z = t.path().join("a.zip");
    assert_eq!(code(&rarc(&["c", &s(&z), &s(&d), "--json"])), 0);
    let o = rarc(&["cv", &s(&z), "--format", "7z", "--json"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stdout));
    let seven = t.path().join("a.7z");
    assert_eq!(json(&o)["output"], s(&seven));
    let v = json(&rarc(&["l", &s(&seven), "--json"]));
    let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"data/sub/b.txt"), "{names:?}");

    let tgz = t.path().join("b.tar.gz");
    let o = rarc(&["cv", &s(&seven), &s(&tgz), "--json"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stdout));
    let out = t.path().join("out");
    assert_eq!(code(&rarc(&["x", &s(&tgz), "-o", &s(&out)])), 0);
    assert_eq!(std::fs::read_to_string(out.join("data/a.txt")).unwrap(), "hello hello hello");
}