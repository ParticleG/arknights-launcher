mod archive;
mod backend;
mod config;
mod files;
mod operations;
mod process;
mod ranges;
mod types;
mod work;

use std::sync::Arc;

use backend::Backend;
use tauri::{AppHandle, State};
use types::{GameStatus, LaunchResult, Operation, ReleaseSummary, Settings};

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("The background command failed: {error}"))?
}

#[tauri::command]
async fn get_settings(state: State<'_, Arc<Backend>>) -> Result<Settings, String> {
    let backend = state.inner().clone();
    blocking(move || backend.get_settings()).await
}

#[tauri::command(rename_all = "snake_case")]
async fn save_settings(settings: Settings, state: State<'_, Arc<Backend>>) -> Result<Settings, String> {
    let backend = state.inner().clone();
    blocking(move || backend.save_settings(settings)).await
}

#[tauri::command]
async fn get_status(state: State<'_, Arc<Backend>>) -> Result<GameStatus, String> {
    let backend = state.inner().clone();
    blocking(move || backend.get_status()).await
}

#[tauri::command]
async fn check_release(state: State<'_, Arc<Backend>>) -> Result<ReleaseSummary, String> {
    let backend = state.inner().clone();
    blocking(move || backend.check_release()).await
}

#[tauri::command]
async fn verify_installation(app: AppHandle, state: State<'_, Arc<Backend>>) -> Result<(), String> {
    let backend = state.inner().clone();
    blocking(move || backend.start(app, Operation::Verify)).await
}

#[tauri::command]
async fn install_or_update(app: AppHandle, state: State<'_, Arc<Backend>>) -> Result<(), String> {
    let backend = state.inner().clone();
    blocking(move || backend.start(app, Operation::Install)).await
}

#[tauri::command]
async fn cancel_operation(state: State<'_, Arc<Backend>>) -> Result<(), String> {
    let backend = state.inner().clone();
    blocking(move || {
        backend.cancel();
        Ok(())
    }).await
}

#[tauri::command]
async fn launch_game(app: AppHandle, state: State<'_, Arc<Backend>>) -> Result<LaunchResult, String> {
    let backend = state.inner().clone();
    blocking(move || backend.launch(app)).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Arc::new(Backend::new()))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            get_status,
            check_release,
            verify_installation,
            install_or_update,
            cancel_operation,
            launch_game,
        ])
        .run(tauri::generate_context!())
        .expect("Unable to run Arknights Launcher");
}
