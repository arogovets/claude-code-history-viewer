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
