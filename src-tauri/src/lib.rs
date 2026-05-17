mod converter;

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_shell::ShellExt;

#[derive(Serialize, Clone)]
struct FileInfo {
    path: String,
    name: String,
    size: u64,
}

#[derive(Serialize)]
struct DiskCheck {
    ok: bool,
    free_bytes: Option<u64>,
    required_bytes: u64,
}

pub struct AppState {
    active_job: Mutex<Option<converter::JobHandle>>,
}

#[tauri::command]
fn inspect_file(path: String) -> Option<FileInfo> {
    let p = PathBuf::from(&path);
    let meta = std::fs::metadata(&p).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(FileInfo {
        name: p.file_name()?.to_string_lossy().into_owned(),
        path,
        size: meta.len(),
    })
}

#[tauri::command]
async fn pick_e57(app: AppHandle) -> Option<FileInfo> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("E57 point cloud", &["e57"])
        .pick_file(move |p| { let _ = tx.send(p); });
    let picked = rx.await.ok().flatten()?;
    let path_buf = picked.into_path().ok()?;
    let path = path_buf.to_string_lossy().into_owned();
    inspect_file(path)
}

#[tauri::command]
async fn pick_output(app: AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .pick_folder(move |p| { let _ = tx.send(p); });
    let picked = rx.await.ok().flatten()?;
    Some(picked.into_path().ok()?.to_string_lossy().into_owned())
}

#[tauri::command]
async fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    app.shell().open(path, None).map_err(|e| e.to_string())
}

#[tauri::command]
fn disk_check(output_dir: String, required_bytes: u64) -> DiskCheck {
    let free = sysinfo::Disks::new_with_refreshed_list()
        .iter()
        .filter(|d| PathBuf::from(&output_dir).starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space());

    let ok = free.map_or(true, |f| f >= required_bytes);
    DiskCheck {
        ok,
        free_bytes: free,
        required_bytes,
    }
}

#[tauri::command]
async fn convert_start(
    app: AppHandle,
    state: State<'_, AppState>,
    input_path: String,
    output_dir: String,
) -> Result<String, String> {
    {
        let guard = state.active_job.lock().unwrap();
        if guard.is_some() {
            return Err("A conversion is already running".into());
        }
    }
    let job_id = uuid::Uuid::new_v4().to_string();
    let handle = converter::start(app, job_id.clone(), input_path, output_dir);
    *state.active_job.lock().unwrap() = Some(handle);
    Ok(job_id)
}

#[tauri::command]
fn convert_cancel(state: State<'_, AppState>) {
    if let Some(h) = state.active_job.lock().unwrap().take() {
        h.cancel();
    }
}

#[tauri::command]
fn job_finished(state: State<'_, AppState>) {
    *state.active_job.lock().unwrap() = None;
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .manage(AppState {
            active_job: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            inspect_file,
            pick_e57,
            pick_output,
            open_path,
            disk_check,
            convert_start,
            convert_cancel,
            job_finished,
        ])
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running scan2bim converter");
}

pub(crate) fn emit_event<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit(event, payload);
}
