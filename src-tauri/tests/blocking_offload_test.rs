//! #516. The heavy read commands were declared `async` while doing entirely
//! synchronous work — `WalkDir`, then mmap and a line scan of every JSONL file
//! — so the async runtime was held for the length of a scan. On a large project
//! that was a visible stall, and under `--serve` a remote caller chose when it
//! happened.
//!
//! A single blocking worker is held until a queued runtime probe releases it.
//! An offloaded command must yield to that probe before its worker can start.
//! This avoids racing a tiny filesystem scan against the runtime scheduler.

mod common;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use claude_code_history_viewer_lib::commands;

/// Hold the only blocking worker until the queued probe gets runtime time.
fn yielded_during<F, T>(f: F) -> bool
where
    F: Future<Output = T>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async move {
            let (release, gate) = std::sync::mpsc::channel();
            let (started, ready) = std::sync::mpsc::channel();
            let worker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = gate.recv();
            });
            ready.recv().unwrap();
            let ran = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&ran);
            let release_from_probe = release.clone();
            tokio::spawn(async move {
                flag.store(true, Ordering::SeqCst);
                let _ = release_from_probe.send(());
            });
            let _ = f.await;
            let result = ran.load(Ordering::SeqCst);
            // Also release the gate for synchronous control commands.
            let _ = release.send(());
            worker.await.unwrap();
            result
        })
}

/// A path that exists but holds nothing. The commands still take their full
/// path through `spawn_blocking`; the assertion is about where the work runs,
/// not how much of it there is.
fn empty_root() -> common::MirrorFixture {
    common::MirrorFixture::new()
}

#[test]
#[serial_test::serial]
fn get_recent_edits_leaves_the_runtime_free() {
    let root = empty_root();
    let path = root.path().to_string_lossy().to_string();

    assert!(
        yielded_during(commands::session::get_recent_edits(
            path, None, None, None, None
        )),
        "get_recent_edits held the runtime thread for the whole scan"
    );
}

#[test]
#[serial_test::serial]
fn scan_projects_leaves_the_runtime_free() {
    let root = empty_root();
    let path = root.path().to_string_lossy().to_string();

    assert!(
        yielded_during(commands::project::scan_projects(path)),
        "scan_projects held the runtime thread for the whole scan"
    );
}

#[test]
#[serial_test::serial]
fn load_project_sessions_page_leaves_the_runtime_free() {
    let root = empty_root();
    let path = root.path().to_string_lossy().to_string();

    assert!(
        yielded_during(commands::session::load_project_sessions_page(
            path, None, None, None
        )),
        "load_project_sessions_page held the runtime thread for the whole load"
    );
}

#[test]
#[serial_test::serial]
fn load_project_sessions_leaves_the_runtime_free() {
    let root = empty_root();
    let path = root.path().to_string_lossy().to_string();

    assert!(
        yielded_during(commands::session::load_project_sessions(path, None)),
        "load_project_sessions held the runtime thread for the whole load"
    );
}

#[test]
#[serial_test::serial]
fn search_messages_leaves_the_runtime_free() {
    let root = empty_root();
    let path = root.path().to_string_lossy().to_string();

    assert!(
        yielded_during(commands::session::search_messages(
            path,
            "needle".to_string(),
            serde_json::json!({}),
            None,
        )),
        "search_messages held the runtime thread for the whole search"
    );
}

/// The control. `get_claude_folder_path` is deliberately *not* offloaded — two
/// stat calls on the home directory, where a task spawn would cost more than it
/// saves. If someone later wraps it, this test says so rather than letting the
/// pattern spread by habit.
#[test]
#[serial_test::serial]
fn trivial_commands_are_deliberately_not_offloaded() {
    assert!(
        !yielded_during(commands::project::get_claude_folder_path()),
        "get_claude_folder_path is now offloading; either that is deliberate \
         (update this test and say why) or the pattern was applied by habit"
    );
}
