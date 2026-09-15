mod common;

use claude_code_history_viewer_lib::{commands, sources};

#[tokio::test]
#[serial_test::serial]
async fn same_native_session_on_two_sources_remains_distinct_and_local() {
    let fixture = common::MirrorFixture::new();
    let root = fixture.path().parent().unwrap().parent().unwrap();
    let second = root.join("second/current");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(
        root.join("second/source.json"),
        r#"{"id":"second","label":"Same label"}"#,
    )
    .unwrap();
    for current in [fixture.path(), second.as_path()] {
        let project = current.join(".claude/projects/-repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("same.jsonl"), concat!(
            r#"{"uuid":"same-message","sessionId":"same","cwd":"/repo","timestamp":"2026-09-12T00:00:00Z","type":"user","message":{"role":"user","content":"mirror needle"}}"#,
            "\n"
        )).unwrap();
    }
    let projects = commands::multi_provider::scan_all_projects(
        Some("/untrusted/live/path".into()),
        Some(vec!["claude".into()]),
        None,
        Some(true),
        None,
    )
    .await
    .unwrap();
    assert_eq!(projects.len(), 2);
    let mut session_ids = std::collections::HashSet::new();
    let mut search_ids = std::collections::HashSet::new();
    for project in &projects {
        let source = sources::resolve(&project.path).unwrap().0;
        let payload = serde_json::to_value(project).unwrap();
        assert_eq!(payload["source_id"], source.id);
        let page = commands::multi_provider::load_provider_sessions_page(
            "claude".into(),
            project.path.clone(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(page.sessions.len(), 1);
        let session = &page.sessions[0];
        assert!(session
            .session_id
            .starts_with(&format!("source:{}|", source.id)));
        session_ids.insert(session.session_id.clone());
        search_ids.insert(format!(
            "source:{}|{}",
            source.id, session.actual_session_id
        ));
        let messages = commands::multi_provider::load_provider_messages(
            "claude".into(),
            session.file_path.clone(),
        )
        .await
        .unwrap();
        assert_eq!(messages.len(), 1);
        assert!(!std::path::Path::new(&project.path)
            .join(".session_cache.json")
            .exists());
    }
    assert_eq!(session_ids.len(), 2);
    let results = commands::multi_provider::search_all_providers(
        None,
        "needle".into(),
        Some(vec!["claude".into()]),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .all(|message| search_ids.contains(&message.session_id)));
    let outside = tempfile::NamedTempFile::new().unwrap();
    assert!(
        commands::session::load_session_messages(outside.path().to_string_lossy().into())
            .await
            .is_err()
    );
    assert!(
        sources::resolve(&format!("source:test|aider://{}", outside.path().display())).is_err()
    );
    assert!(sources::resolve(&format!("source:test|crush://{}#id", second.display())).is_err());
    assert!(commands::session::delete_session(projects[0].path.clone())
        .await
        .is_err());
}

/// Exercise the real collector layout and the production (non-cfg(test)) scan API.
#[tokio::test]
#[serial_test::serial]
async fn collector_manifest_to_inventory_to_provider_scans() {
    use std::{fs, process::Command};
    let fixture = common::MirrorFixture::new();
    let origin = tempfile::tempdir().unwrap();
    let home = origin.path();
    let write = |relative: &str, body: &str| {
        let path = home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    };
    write(
        ".claude/projects/-repo/session.jsonl",
        concat!(
            r#"{"uuid":"message","sessionId":"session","cwd":"/repo","timestamp":"2026-09-12T00:00:00Z","type":"user","message":{"role":"user","content":"mirror"}}"#,
            "\n"
        ),
    );
    write(
        ".codex/sessions/2026/09/12/rollout-2026-09-12T00-00-00-session.jsonl",
        concat!(
            r#"{"type":"session_meta","timestamp":"2026-09-12T00:00:00Z","payload":{"id":"session","cwd":"/repo","timestamp":"2026-09-12T00:00:00Z"}}"#,
            "\n",
            r#"{"type":"response_item","timestamp":"2026-09-12T00:00:01Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"mirror"}]}}"#,
            "\n"
        ),
    );
    write(
        ".local/share/opencode/storage/project/project.json",
        r#"{"id":"project","worktree":"/repo"}"#,
    );
    write(
        ".local/share/opencode/storage/session/project/session.json",
        r#"{"id":"session","projectID":"project","directory":"/repo","title":"Mirror","time":{"created":1750000000000,"updated":1750000000000}}"#,
    );
    write(
        "arbitrary/nested/team/repo/.aider.chat.history.md",
        "# aider chat started at 2026-09-12 00:00:00\n\n#### mirror\n\nhello\n",
    );
    write(
        "arbitrary/nested/team/repo/private-code.txt",
        "do not collect",
    );
    fs::create_dir_all(home.join("arbitrary/nested/team/repo/.crush")).unwrap();
    let db = rusqlite::Connection::open(home.join("arbitrary/nested/team/repo/.crush/crush.db"))
        .unwrap();
    db.execute_batch("CREATE TABLE sessions(id TEXT, updated_at INTEGER); INSERT INTO sessions VALUES ('same', 1750000000);").unwrap();
    drop(db);
    // Include Linux storage on a macOS viewer to guard against current-host OS routing.
    let db_path = home.join(".local/share/amazon-q/data.sqlite3");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(db_path).unwrap();
    db.execute_batch("CREATE TABLE conversations(key TEXT, value TEXT);")
        .unwrap();
    drop(db);
    let root = fixture.path().parent().unwrap().parent().unwrap();
    let source = serde_json::json!({"id":"collected", "label":"Collected", "home":home,
        "project_roots":[{"path":home.join("arbitrary"),"mirror_path":"work/custom"}]});
    let script = r"
import json, pathlib, sys, subprocess
sys.path.insert(0, sys.argv[1])
import collect
# Restic itself is covered by the collector's real-repository integration test.
collect.run = lambda args: None if args[0] == 'restic' else subprocess.run(args, check=True)
collect.collect_source(pathlib.Path(sys.argv[2]), json.loads(sys.argv[3]))
";
    let result = Command::new("python3")
        .args(["-c", script])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../collector"))
        .arg(root)
        .arg(source.to_string())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let inventory = commands::multi_provider::list_filesystem_sources()
        .await
        .unwrap();
    let collected = inventory.iter().find(|s| s.id == "collected").unwrap();
    assert!(collected.last_collected_at.is_some());
    assert_eq!(collected.origin.home.as_deref(), home.to_str());
    assert!(collected
        .mounts
        .iter()
        .any(|m| m.providers == ["aider", "crush"]));
    assert!(!collected
        .current
        .join("work/custom/nested/team/repo/private-code.txt")
        .exists());
    // Removing the entire origin proves all subsequent detection/parsing uses current.
    fs::remove_dir_all(home).unwrap();
    let expected = ["claude", "codex", "opencode", "aider", "crush"];
    let projects = commands::multi_provider::scan_all_projects(
        Some("/untrusted".into()),
        Some(expected.iter().map(|s| (*s).into()).collect()),
        Some(vec![commands::multi_provider::CustomClaudePathParam {
            path: "/untrusted".into(),
            label: None,
        }]),
        None,
        None,
    )
    .await
    .unwrap();
    for provider in expected {
        assert!(
            projects
                .iter()
                .any(|p| p.provider.as_deref() == Some(provider)),
            "missing {provider}: {projects:?}"
        );
    }
    let infos = commands::multi_provider::detect_providers().await.unwrap();
    assert!(infos.iter().any(|p| p.id == "amazonq" && p.is_available));
    for project in projects {
        assert_eq!(sources::resolve(&project.path).unwrap().0.id, "collected");
    }
}

#[tokio::test]
#[serial_test::serial]
async fn inventory_includes_pending_sources_and_rejects_traversing_mounts() {
    let fixture = common::MirrorFixture::new();
    let source_dir = fixture.path().parent().unwrap();
    let metadata = source_dir.join("source.json");
    std::fs::remove_dir(fixture.path()).unwrap();
    let mut value = serde_json::json!({"id":"test","label":"Remote","origin":{"ssh":"host", "paths":[
        {"path":"/home/me/.codex", "mirror_path":".codex", "kind":"directory"}
    ]}});
    std::fs::write(&metadata, value.to_string()).unwrap();
    let inventory = commands::multi_provider::list_filesystem_sources()
        .await
        .unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].origin.ssh.as_deref(), Some("host"));
    assert_eq!(inventory[0].mounts[0].providers, ["codex"]);
    assert!(sources::list().unwrap().is_empty());
    value["origin"]["paths"][0]["mirror_path"] = "../escape".into();
    std::fs::write(&metadata, value.to_string()).unwrap();
    assert!(commands::multi_provider::list_filesystem_sources()
        .await
        .is_err());
}

#[tokio::test]
#[serial_test::serial]
async fn source_qualified_opencode_subagents_use_the_selected_mirror() {
    let fixture = common::MirrorFixture::new();
    let root = fixture.path().parent().unwrap().parent().unwrap();
    let second = root.join("second/current");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(
        root.join("second/source.json"),
        r#"{"id":"second","label":"Second"}"#,
    )
    .unwrap();
    for (current, title) in [
        (fixture.path(), "First source"),
        (second.as_path(), "Second source"),
    ] {
        let store = current.join(".local/share/opencode");
        std::fs::create_dir_all(&store).unwrap();
        let conn = rusqlite::Connection::open(store.join("opencode.db")).unwrap();
        conn.execute_batch("CREATE TABLE session (id TEXT, project_id TEXT, title TEXT, time_created INTEGER, time_updated INTEGER, parent_id TEXT);
            CREATE TABLE message (id TEXT, session_id TEXT);").unwrap();
        conn.execute("INSERT INTO session VALUES ('child', 'project', ?1, 1700000000000, 1700000000000, 'ses_f80dca67dffeGCfsH0qXK3RANQ')", [title]).unwrap();
    }
    for (id, title) in [("test", "First source"), ("second", "Second source")] {
        let parent = format!("source:{id}|opencode://project/ses_f80dca67dffeGCfsH0qXK3RANQ");
        let children = commands::session::get_session_subagents(parent)
            .await
            .unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].summary.as_deref(), Some(title));
        assert_eq!(
            children[0].file_path,
            format!("source:{id}|opencode://project/child")
        );
        assert!(
            commands::session::get_session_subagents(children[0].file_path.clone())
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        commands::session::get_session_subagents("source:test|codex://project/session".into())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(commands::session::get_session_subagents(
        "source:missing|opencode://project/session".into()
    )
    .await
    .is_err());
    assert!(commands::session::get_session_subagents(
        "source:test|opencode://project/../escape".into()
    )
    .await
    .is_err());
    assert!(
        commands::session::get_session_subagents("/outside/session.jsonl".into())
            .await
            .is_err()
    );
}
