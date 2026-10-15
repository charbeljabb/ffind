//! End-to-end tests: run the real `ffind` binary against temporary folder trees.
//!
//! stdout is piped here (not a terminal), so ffind prints plain full paths,
//! one per line, with no colors. The spinner draws to stderr and stays hidden.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

/// A deadlock should fail the test, not hang `cargo test` forever.
const TIMEOUT: Duration = Duration::from_secs(10);

struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Every printed line, as a path.
    fn lines(&self) -> Vec<PathBuf> {
        self.stdout.lines().map(PathBuf::from).collect()
    }

    /// The printed paths, ignoring order (threads print in any order).
    fn found(&self) -> BTreeSet<PathBuf> {
        self.lines().into_iter().collect()
    }
}

fn run(args: &[&str]) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ffind"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start ffind");

    // Read output on separate threads. If we only waited, ffind could fill the
    // pipe buffer and block on a write, which would look like a hang.
    let stdout = read_in_background(child.stdout.take().unwrap());
    let stderr = read_in_background(child.stderr.take().unwrap());

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            panic!("ffind {args:?} still running after {TIMEOUT:?}, probably a hang");
        }
        thread::sleep(Duration::from_millis(10));
    };

    Run {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

fn read_in_background(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Search `root` for `target`.
fn search(target: &str, root: &TempDir) -> Run {
    run(&[target, root.path().to_str().unwrap()])
}

/// Create an empty file, plus any missing parent folders.
fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "").unwrap();
}

fn set<const N: usize>(paths: [PathBuf; N]) -> BTreeSet<PathBuf> {
    paths.into_iter().collect()
}

#[test]
fn finds_file_in_start_folder() {
    let root = tempdir().unwrap();
    let file = root.path().join("needle.txt");
    touch(&file);

    let out = search("needle", &root);

    assert!(out.status.success());
    assert_eq!(out.found(), set([file]));
}

#[test]
fn finds_file_deep_in_tree() {
    let root = tempdir().unwrap();
    let file = root.path().join("a").join("b").join("c").join("needle.rs");
    touch(&file);

    let out = search("needle", &root);

    assert_eq!(out.found(), set([file]));
}

#[test]
fn finds_matches_in_sibling_folders() {
    let root = tempdir().unwrap();
    let one = root.path().join("left").join("needle1.txt");
    let two = root.path().join("right").join("deeper").join("needle2.txt");
    touch(&one);
    touch(&two);
    touch(&root.path().join("middle").join("hay.txt"));

    let out = search("needle", &root);

    assert_eq!(out.found(), set([one, two]));
}

#[test]
fn matches_folder_names_and_still_searches_inside_them() {
    let root = tempdir().unwrap();
    let folder = root.path().join("needle_folder");
    let inside = folder.join("needle_inside.txt");
    touch(&inside);

    let out = search("needle", &root);

    assert_eq!(out.found(), set([folder, inside]));
}

#[test]
fn matches_text_anywhere_in_the_name() {
    let root = tempdir().unwrap();
    let start = root.path().join("needle_at_start.txt");
    let middle = root.path().join("has_needle_inside.txt");
    let end = root.path().join("ends_with.needle");
    touch(&start);
    touch(&middle);
    touch(&end);

    let out = search("needle", &root);

    assert_eq!(out.found(), set([start, middle, end]));
}

#[test]
fn matching_is_case_sensitive() {
    let root = tempdir().unwrap();
    touch(&root.path().join("NEEDLE.txt"));
    touch(&root.path().join("Needle.txt"));

    let out = search("needle", &root);

    assert!(out.status.success());
    assert!(
        out.found().is_empty(),
        "unexpected matches: {:?}",
        out.found()
    );
}

#[test]
fn only_names_are_matched_not_the_folder_path() {
    let root = tempdir().unwrap();
    let folder = root.path().join("needle");
    touch(&folder.join("hay.txt"));

    let out = search("needle", &root);

    // The folder itself matches, but `hay.txt` doesn't just because it lives in it.
    assert_eq!(out.found(), set([folder]));
}

#[test]
fn no_matches_prints_nothing_and_succeeds() {
    let root = tempdir().unwrap();
    touch(&root.path().join("a").join("hay.txt"));

    let out = search("needle", &root);

    assert!(out.status.success());
    assert_eq!(out.stdout, "");
}

#[test]
fn empty_start_folder_finishes() {
    let root = tempdir().unwrap();

    let out = search("needle", &root);

    assert!(out.status.success());
    assert_eq!(out.stdout, "");
}

#[test]
fn missing_start_folder_does_not_hang() {
    let root = tempdir().unwrap();
    let missing = root.path().join("does-not-exist");

    let out = run(&["needle", missing.to_str().unwrap()]);

    assert!(out.status.success());
    assert_eq!(out.stdout, "");
}

#[test]
fn many_folders_every_match_printed_exactly_once() {
    // Lots of folders keep all worker threads busy at once, which is where
    // lost or duplicated work would show up.
    let root = tempdir().unwrap();
    let mut expected = BTreeSet::new();
    for i in 0..50 {
        for j in 0..10 {
            let dir = root.path().join(format!("dir{i}")).join(format!("sub{j}"));
            let file = dir.join(format!("needle_{i}_{j}.txt"));
            touch(&file);
            touch(&dir.join("hay.txt"));
            expected.insert(file);
        }
    }

    let out = search("needle", &root);
    let lines = out.lines();

    assert_eq!(out.found(), expected, "missing or extra matches");
    assert_eq!(
        lines.len(),
        expected.len(),
        "some matches were printed twice"
    );
}

#[test]
fn piped_output_is_plain_full_paths() {
    let root = tempdir().unwrap();
    let file = root.path().join("sub").join("needle.txt");
    touch(&file);

    let out = search("needle", &root);

    assert!(!out.stdout.contains('\x1b'), "color codes in piped output");
    assert_eq!(out.stdout.trim_end(), file.to_str().unwrap());
}

#[test]
fn missing_search_text_is_a_usage_error() {
    let out = run(&[]);

    assert!(!out.status.success());
    assert!(out.stderr.contains("Usage"), "stderr was: {}", out.stderr);
}
