mod app;
mod core;
mod env;
mod k8s;
mod ui;

pub use core::*;

use std::io;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::app::{App, PendingExternal};
use crate::core::commands::dyn_api;
use crate::core::state::AppState;

struct CliArgs {
    context: Option<String>,
    kubeconfig: Option<String>,
}

fn parse_args() -> CliArgs {
    let mut args = std::env::args().skip(1);
    let mut context = None;
    let mut kubeconfig = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "steerglass-tui {}\n\nUSAGE:\n  steerglass-tui [OPTIONS]\n\nOPTIONS:\n  -c, --context <NAME>       Open directly on a cluster context\n      --kubeconfig <PATH>    Path to kubeconfig file\n  -h, --help                 Print help\n  -V, --version              Print version",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("steerglass-tui {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-c" | "--context" => {
                context = args.next();
            }
            "--kubeconfig" => {
                kubeconfig = args.next();
            }
            other => {
                eprintln!("Unknown argument: {other}\nRun with --help for usage.");
                std::process::exit(2);
            }
        }
    }
    CliArgs { context, kubeconfig }
}

fn settings_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("com.steerglass.app")
        .join("settings.json")
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        prev(info);
    }));
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = parse_args();
    if let Some(kc) = cli.kubeconfig {
        // Safe before spawning background threads.
        unsafe { std::env::set_var("KUBECONFIG", kc) };
    }

    core::kubeconfig::fix_path_env();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let state = Arc::new(AppState::new(settings_path()));
    let (tx, mut rx) = mpsc::unbounded_channel();

    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(state.clone(), tx.clone(), cli.context);

    let res = run_loop(&mut terminal, &mut app, &mut rx).await;

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    res
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<k8s::AppEvent>,
) -> io::Result<()> {
    loop {
        while let Ok(ev) = rx.try_recv() {
            app.handle_event(ev);
        }

        terminal.draw(|f| ui::render(f, app))?;

        if let Some(ext) = app.pending_external.take() {
            run_external(terminal, app, ext).await?;
            continue;
        }

        if app.should_quit {
            break;
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    app.handle_key(key);
                }
            }
        }
        app.tick_count = app.tick_count.wrapping_add(1);
    }
    Ok(())
}

async fn run_external(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    ext: PendingExternal,
) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    match ext {
        PendingExternal::Shell {
            context,
            namespace,
            pod,
            container,
        } => {
            let mut cmd = Command::new("kubectl");
            cmd.arg("--context")
                .arg(&context)
                .arg("exec")
                .arg("-it")
                .arg("-n")
                .arg(&namespace)
                .arg(&pod);
            if let Some(c) = container {
                cmd.arg("-c").arg(c);
            }
            cmd.arg("--")
                .arg("sh")
                .arg("-c")
                .arg("command -v bash >/dev/null 2>&1 && exec bash || exec sh");

            match cmd.status() {
                Ok(status) if status.success() => {}
                Ok(status) => app.set_toast(format!("Shell exited with {status}"), true),
                Err(e) => app.set_toast(format!("Failed to launch kubectl: {e}"), true),
            }
        }
        PendingExternal::EditYaml {
            context,
            kind,
            namespace,
            name,
            yaml,
        } => {
            let initial_yaml = if yaml.is_empty() {
                match app.state.client(&context).await {
                    Ok(client) => {
                        let api = dyn_api(client, &kind, namespace.as_deref());
                        match api.get(&name).await {
                            Ok(obj) => {
                                let mut v = serde_json::to_value(obj).unwrap_or_default();
                                if let Some(meta) = v.get_mut("metadata").and_then(|m| m.as_object_mut()) {
                                    meta.remove("managedFields");
                                }
                                serde_yaml::to_string(&v).unwrap_or_default()
                            }
                            Err(e) => {
                                app.set_toast(format!("Failed to fetch YAML: {e}"), true);
                                String::new()
                            }
                        }
                    }
                    Err(e) => {
                        app.set_toast(format!("Client error: {e}"), true);
                        String::new()
                    }
                }
            } else {
                yaml
            };

            if !initial_yaml.is_empty() {
                let tmp = std::env::temp_dir().join(format!("steerglass-{name}.yaml"));
                std::fs::write(&tmp, &initial_yaml)?;
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
                let status = Command::new(&editor).arg(&tmp).status();
                match status {
                    Ok(s) if s.success() => {
                        let updated = std::fs::read_to_string(&tmp).unwrap_or_default();
                        if updated != initial_yaml && !updated.trim().is_empty() {
                            k8s::spawn_replace_yaml(
                                app.state.clone(),
                                context,
                                kind,
                                namespace,
                                name,
                                updated,
                                app.tx.clone(),
                            );
                        } else {
                            app.set_toast("No changes made", false);
                        }
                    }
                    Ok(s) => app.set_toast(format!("Editor exited with {s}"), true),
                    Err(e) => app.set_toast(format!("Failed to launch {editor}: {e}"), true),
                }
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(())
}
