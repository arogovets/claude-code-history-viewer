//! Project organization, stored separately from provider history and user metadata.
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, io::Write, path::Path, sync::Mutex};

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

#[tauri::command]
pub async fn load_kanban() -> Result<KanbanData, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let _guard = KANBAN_LOCK.lock().map_err(|e| e.to_string())?;
        let path = super::metadata::get_user_data_path()?.with_file_name("boards.json");
        read_data(&path)
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
