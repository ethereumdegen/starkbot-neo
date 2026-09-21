//! Starkbot Neo, the desktop app shell (P1, P12).
//!
//! One window over the whole product: the thread and the agent turn it
//! starts, the navigator, the accessibility surface, the eval suite,
//! settings, and the Connections screen the first run begins with.
//! Everything below this crate is Tauri-free (A1); this crate holds no
//! vendor knowledge and no secret, and reaches the product only through
//! `neo_agent::Runtime`.
#![forbid(unsafe_code)]
// A desktop app has no stdout to speak on; the few lifecycle lines it writes
// go to stderr, where `cargo tauri dev` shows them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bindings;
mod commands;
mod error;
mod events;
mod state;
mod view;

#[cfg(test)]
mod tests;

use state::{Desktop, default_data_dir};

fn main() -> std::process::ExitCode {
    // Named before anything is opened, so a span produced while the runtime
    // is starting still belongs to this process. Tauri's async runtime is
    // entered for the call because the exporter is a tokio task and `init`
    // needs a runtime handle to spawn it on; that runtime outlives the
    // window, which is what the exporter needs.
    tauri::async_runtime::block_on(async { neo_otel::init("neo-desktop") });
    let Some(data_dir) = default_data_dir() else {
        eprintln!("neo-desktop: HOME is not set, so there is nowhere to keep the store");
        return std::process::ExitCode::from(2);
    };
    let desktop = match Desktop::open(&data_dir) {
        Ok(desktop) => desktop,
        Err(error) => {
            eprintln!(
                "neo-desktop: could not open the runtime at {}: {error}",
                data_dir.display()
            );
            return std::process::ExitCode::from(2);
        }
    };
    eprintln!("neo-desktop: runtime open at {}", data_dir.display());

    let app = tauri::Builder::default()
        .manage(desktop)
        .invoke_handler(tauri::generate_handler![
            commands::handshake,
            commands::get_bootstrap,
            commands::list_projects,
            commands::create_project,
            commands::show_project,
            commands::save_project_document,
            commands::configure_project_heartbeat,
            commands::run_project_heartbeat,
            commands::connections,
            commands::begin_login,
            commands::open_login_page,
            commands::finish_login_pasted,
            commands::cancel_login,
            commands::disconnect,
            commands::run_doctor,
            commands::key_status,
            commands::set_key,
            commands::check_key,
            commands::remove_key,
            commands::set_inference_runtime,
            commands::list_models,
            commands::refresh_models,
            commands::get_settings,
            commands::patch_settings,
            commands::list_conversations,
            commands::new_conversation,
            commands::rename_conversation,
            commands::load_thread,
            commands::send_message,
            commands::steer_run,
            commands::stop_run,
            commands::resolve_confirm,
            commands::answer_ask,
            commands::run_nav,
            commands::run_app_goal,
            commands::run_ax,
            commands::list_eval_cases,
            commands::run_eval,
        ])
        .setup(|app| {
            // Subscribed before the window can invoke anything, so the first
            // turn a screen starts cannot outrun the stream that reports it.
            let runtime = tauri::Manager::state::<Desktop>(app).runtime();
            let scheduler = runtime.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = scheduler.start_heartbeat_scheduler().await {
                    eprintln!("neo-desktop: heartbeat scheduler stopped: {error}");
                }
            });
            events::forward(app.handle().clone(), &runtime);
            eprintln!("neo-desktop: window `main` created");
            Ok(())
        })
        .run(tauri::generate_context!());

    // The window is closed and the last screen's spans are still in the
    // queue; the desktop has a runtime to wait on, so it waits.
    tauri::async_runtime::block_on(neo_otel::shutdown());
    match app {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("neo-desktop: the app could not start: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
