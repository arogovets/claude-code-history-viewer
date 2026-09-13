//! Project organization, stored separately from provider history and user metadata.
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::Path,
    sync::Mutex,
};

static KANBAN_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KanbanData {
    pub revision: u64,
    pub boards: Vec<KanbanBoard>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KanbanBoard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub id: String,
    pub name: String,
    /// Array order is the display order. Each project appears once per board.
    pub columns: Vec<KanbanColumn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KanbanColumn {
    pub id: String,
    pub name: String,
    /// Ordered references to existing projects: JSON-encoded [provider, storage path].
    pub project_ids: Vec<String>,
}

fn validate(data: &KanbanData) -> Result<(), String> {
    let mut board_ids = HashSet::new();
    for board in &data.boards {
        if let Some(color) = &board.color {
            if color.len() != 7
                || !color.starts_with('#')
                || !color[1..].chars().all(|c| c.is_ascii_hexdigit())
            {
                return Err("Board color must be a hex color".into());
            }
        }
        if board.id.trim().is_empty() || !board_ids.insert(&board.id) {
            return Err("Board IDs must be nonempty and unique".into());
        }
        if board.name.trim().is_empty() || board.columns.is_empty() {
            return Err("Each board needs a name and at least one column".into());
        }
        let mut column_ids = HashSet::new();
        let mut projects = HashSet::new();
        for column in &board.columns {
            if column.id.trim().is_empty()
                || !column_ids.insert(&column.id)
                || column.name.trim().is_empty()
            {
                return Err(
                    "Column IDs must be unique within a board and columns must have names".into(),
                );
            }
            for project in &column.project_ids {
                let key: [String; 2] = serde_json::from_str(project)
                    .map_err(|_| "Invalid project reference".to_string())?;
                if key.iter().any(|part| part.trim().is_empty()) || !projects.insert(project) {
                    return Err("A project may appear only once per board".into());
                }
            }
        }
    }
    Ok(())
}

fn read_data(path: &Path) -> Result<KanbanData, String> {
    match fs::read_to_string(path) {
        Ok(content) => {
            let data =
                serde_json::from_str(&content).map_err(|e| format!("Invalid boards file: {e}"))?;
            validate(&data)?;
            Ok(data)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KanbanData::default()),
        Err(e) => Err(format!("Failed to read boards: {e}")),
    }
}

fn write_data(path: &Path, mut data: KanbanData) -> Result<KanbanData, String> {
    validate(&data)?;
    let current = read_data(path)?;
    if current.revision != data.revision {
        return Err("Boards changed in another window. Reload boards and try again.".into());
    }
    data.revision = current
        .revision
        .checked_add(1)
        .ok_or("Board revision overflow")?;
    let parent = path.parent().ok_or("Invalid boards path")?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to create boards folder: {e}"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(&data).map_err(|e| e.to_string())?;
    temp.write_all(&bytes)
        .map_err(|e| format!("Failed to write boards: {e}"))?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist(path)
        .map_err(|e| format!("Failed to save boards: {e}"))?;
    Ok(data)
}

/// Unique aliases only: identical native handles on two sources must not guess.
fn migrate_references(data: &mut KanbanData, aliases: &HashMap<String, HashSet<String>>) {
    for board in &mut data.boards {
        let mut seen = HashSet::new();
        for column in &mut board.columns {
            for id in &mut column.project_ids {
                if let Some(matches) = aliases.get(id).filter(|matches| matches.len() == 1) {
                    *id = matches.iter().next().unwrap().clone();
                }
            }
            column.project_ids.retain(|id| seen.insert(id.clone()));
        }
    }
}

fn save_migrated(
    path: &Path,
    aliases: &HashMap<String, HashSet<String>>,
) -> Result<KanbanData, String> {
    let current = read_data(path)?;
    let mut migrated = current.clone();
    migrate_references(&mut migrated, aliases);
    if migrated == current {
        return Ok(current);
    }
    // Keep the exact pre-migration bytes; never replace an existing backup.
    let backup = path.with_file_name(format!("boards.before-sources-{}.json", current.revision));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup)
    {
        Ok(mut file) => {
            file.write_all(&fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("Cannot back up boards before migration: {e}")),
    }
    write_data(path, migrated)
}

#[tauri::command]
pub async fn load_kanban() -> Result<KanbanData, String> {
    let data = tauri::async_runtime::spawn_blocking(|| {
        let _guard = KANBAN_LOCK.lock().map_err(|e| e.to_string())?;
        let path = super::metadata::get_user_data_path()?.with_file_name("boards.json");
        read_data(&path)
    })
    .await
    .map_err(|e| e.to_string())??;
    if data
        .boards
        .iter()
        .all(|b| b.columns.iter().all(|c| c.project_ids.is_empty()))
    {
        return Ok(data);
    }
    let projects =
        match super::multi_provider::scan_all_projects(None, None, None, None, None).await {
            Ok(projects) => projects,
            Err(_) => return Ok(data), // Offline/unavailable sources never hide saved boards.
        };
    let mut aliases: HashMap<String, HashSet<String>> = HashMap::new();
    for project in projects {
        let Ok((source, _)) = crate::sources::resolve(&project.path) else {
            continue;
        };
        let Some(legacy) = crate::sources::legacy_local_project_path(&source, &project.path) else {
            continue;
        };
        let provider = project.provider.as_deref().unwrap_or("claude");
        let old = serde_json::to_string(&[provider, &legacy]).map_err(|e| e.to_string())?;
        let new = serde_json::to_string(&[provider, &project.path]).map_err(|e| e.to_string())?;
        aliases.entry(old).or_default().insert(new);
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = KANBAN_LOCK.lock().map_err(|e| e.to_string())?;
        let path = super::metadata::get_user_data_path()?.with_file_name("boards.json");
        save_migrated(&path, &aliases)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn save_kanban(data: KanbanData) -> Result<KanbanData, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = KANBAN_LOCK.lock().map_err(|e| e.to_string())?;
        let path = super::metadata::get_user_data_path()?.with_file_name("boards.json");
        write_data(&path, data)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> KanbanData {
        serde_json::from_value(serde_json::json!({"revision": 0, "boards": [
            {"id": "a", "name": "Work", "columns": [
                {"id": "todo", "name": "To do", "projectIds": ["[\"claude\",\"/project\"]"]},
                {"id": "done", "name": "Done", "projectIds": []}
            ]},
            {"id": "b", "name": "Personal", "columns": [
                {"id": "done", "name": "Done", "projectIds": ["[\"claude\",\"/project\"]"]}
            ]}
        ]}))
        .unwrap()
    }

    #[test]
    fn migration_preserves_boards_order_and_ambiguous_references() {
        let mut data = fixture();
        let old = data.boards[0].columns[0].project_ids[0].clone();
        let new = r#"["claude","/mirrors/local/current/project"]"#.to_string();
        let mut aliases = HashMap::from([(old.clone(), HashSet::from([new.clone()]))]);
        migrate_references(&mut data, &aliases);
        assert_eq!(data.boards[0].columns[0].project_ids, vec![new.clone()]);
        assert_eq!(data.boards[1].columns[0].project_ids, vec![new.clone()]);
        assert!(data.boards[0].columns[1].project_ids.is_empty());
        aliases
            .get_mut(&old)
            .unwrap()
            .insert("another source".into());
        let mut ambiguous = fixture();
        migrate_references(&mut ambiguous, &aliases);
        assert_eq!(ambiguous, fixture());
        let mut duplicate = fixture();
        duplicate.boards[0].columns[1].project_ids.push(new.clone());
        aliases.get_mut(&old).unwrap().remove("another source");
        migrate_references(&mut duplicate, &aliases);
        assert!(duplicate.boards[0].columns[1].project_ids.is_empty());
    }

    #[test]
    fn migration_backs_up_exact_data_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boards.json");
        let original = serde_json::to_vec(&fixture()).unwrap();
        fs::write(&path, &original).unwrap();
        let old = fixture().boards[0].columns[0].project_ids[0].clone();
        let new = r#"["claude","/mirrors/local/current/project"]"#.to_string();
        let aliases = HashMap::from([(old, HashSet::from([new]))]);
        let migrated = save_migrated(&path, &aliases).unwrap();
        assert_eq!(migrated.revision, 1);
        assert_eq!(
            fs::read(dir.path().join("boards.before-sources-0.json")).unwrap(),
            original
        );
        assert_eq!(save_migrated(&path, &aliases).unwrap(), migrated);
        assert!(write_data(&path, fixture()).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn round_trip_and_stale_write_protection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boards.json");
        assert_eq!(read_data(&path).unwrap(), KanbanData::default());
        let saved = write_data(&path, fixture()).unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(read_data(&path).unwrap(), saved);
        assert!(write_data(&path, fixture())
            .unwrap_err()
            .contains("another window"));
        assert_eq!(read_data(&path).unwrap(), saved);
    }

    #[test]
    #[serial_test::serial]
    fn rejects_duplicate_membership_but_allows_multiple_boards() {
        let mut data = fixture();
        assert!(validate(&data).is_ok());
        data.boards[0].columns[1].project_ids = data.boards[0].columns[0].project_ids.clone();
        assert!(validate(&data).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn invalid_data_never_overwrites_saved_boards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boards.json");
        let saved = write_data(&path, fixture()).unwrap();
        let mut invalid = saved.clone();
        invalid.boards[0].columns.clear();
        assert!(write_data(&path, invalid).is_err());
        assert_eq!(read_data(&path).unwrap(), saved);
        fs::write(&path, "invalid json").unwrap();
        assert!(write_data(&path, fixture()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "invalid json");
    }
}
