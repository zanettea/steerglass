use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;

use crate::core::health::{Issue, Scan};
use crate::core::kubeconfig::{ContextInfo, TuiViewState};
use crate::core::rows::{Health, Row};
use crate::core::state::{AppState, ForwardInfo, KindRef};
use crate::env::{Env, EnvChoice, resolve_env};
use crate::k8s::{self, AppEvent, ClusterSummary, WatchUpdate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Resources,
    Utilization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtilizationPane {
    Nodes,
    Pods,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtilizationSort {
    CpuDesc,
    MemDesc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Main,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewPane {
    Clusters,
    Issues,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceSort {
    NameAsc,
    AgeNewestFirst,
    AgeOldestFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragTarget {
    Sidebar,
    OverviewSplit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YamlViewMode {
    Raw,
    Decoded,
    Interpreted,
}

pub const DEFAULT_SIDEBAR_WIDTH: u16 = 30;
pub const DEFAULT_OVERVIEW_SPLIT_PCT: u16 = 48;

#[derive(Debug, Clone)]
pub struct TableRow {
    pub context: String,
    pub row: Row,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortForwardTarget {
    pub bind_addr: String,
    pub local_port: Option<u16>,
    pub remote_port: u16,
}

pub fn parse_port_forward_input(
    input: &str,
    default_remote: Option<u16>,
) -> std::result::Result<PortForwardTarget, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return match default_remote {
            Some(remote_port) => Ok(PortForwardTarget {
                bind_addr: "127.0.0.1".into(),
                local_port: None,
                remote_port,
            }),
            None => Err("Enter a port (e.g. 8080, 8080:80, or 0.0.0.0:8080:80)".into()),
        };
    }

    // Check if input is just a bind IP (e.g. "0.0.0.0") when a default remote port is selected.
    if let Ok(ip) = trimmed
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
    {
        if let Some(remote_port) = default_remote {
            return Ok(PortForwardTarget {
                bind_addr: ip.to_string(),
                local_port: None,
                remote_port,
            });
        }
    }

    // Handle "[ipv6]:local:remote" or "ipv4:local:remote" or "local:remote" or "remote".
    let (bind_addr, rest) = if let Some(after_bracket) = trimmed.strip_prefix('[') {
        let Some((addr, tail)) = after_bracket.split_once("]:") else {
            return Err("Invalid IPv6 bind syntax (use [::1]:8080:80)".into());
        };
        let _: std::net::IpAddr = addr
            .parse()
            .map_err(|_| format!("Invalid IPv6 bind address: {addr}"))?;
        (addr.to_string(), tail)
    } else {
        let parts: Vec<&str> = trimmed.split(':').collect();
        if parts.len() == 3 {
            let _: std::net::IpAddr = parts[0]
                .trim()
                .parse()
                .map_err(|_| format!("Invalid bind address: {}", parts[0]))?;
            (
                parts[0].trim().to_string(),
                &trimmed[parts[0].len() + 1..],
            )
        } else if parts.len() <= 2 {
            ("127.0.0.1".to_string(), trimmed)
        } else {
            return Err("Invalid format (use 8080, 8080:80, or 0.0.0.0:8080:80)".into());
        }
    };

    if let Some((local_s, remote_s)) = rest.split_once(':') {
        let local = if local_s.trim().is_empty() || local_s.trim() == "0" {
            None
        } else {
            let p: u16 = local_s
                .trim()
                .parse()
                .map_err(|_| "Invalid local port".to_string())?;
            Some(p)
        };
        let remote: u16 = if remote_s.trim().is_empty() {
            default_remote.ok_or_else(|| "Invalid remote port".to_string())?
        } else {
            remote_s
                .trim()
                .parse()
                .map_err(|_| "Invalid remote port".to_string())?
        };
        if remote == 0 {
            return Err("Remote port must be > 0".into());
        }
        Ok(PortForwardTarget {
            bind_addr,
            local_port: local,
            remote_port: remote,
        })
    } else {
        let remote: u16 = rest
            .trim()
            .parse()
            .map_err(|_| "Invalid port (use 8080, 8080:80, or 0.0.0.0:8080:80)".to_string())?;
        if remote == 0 {
            return Err("Port must be > 0".into());
        }
        Ok(PortForwardTarget {
            bind_addr,
            local_port: None,
            remote_port: remote,
        })
    }
}

pub fn yaml_lines_for_mode(raw: &[String], mode: YamlViewMode) -> Vec<String> {
    match mode {
        YamlViewMode::Raw => raw.to_vec(),
        YamlViewMode::Decoded => decode_yaml_data_lines(raw),
        YamlViewMode::Interpreted => crate::interpret::interpret_yaml_lines(raw),
    }
}

pub fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    use base64::Engine;
    use parking_lot::Mutex;

    // Keep arboard::Clipboard alive for the process lifetime (required on X11/Wayland).
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    let mut copied_native = false;
    let guard = CLIPBOARD.get_or_init(|| Mutex::new(arboard::Clipboard::new().ok()));
    if let Some(cb) = guard.lock().as_mut() {
        if cb.set_text(text.to_string()).is_ok() {
            copied_native = true;
        }
    }

    if !copied_native {
        let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
            &[("pbcopy", &[])]
        } else {
            &[
                ("wl-copy", &[]),
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--input"]),
            ]
        };
        for (bin, args) in candidates {
            if let Ok(mut child) = Command::new(bin)
                .args(*args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                if child.wait().is_ok_and(|s| s.success()) {
                    break;
                }
            }
        }
    }

    // Also emit OSC 52 (works over SSH / modern terminals even without X11/Wayland).
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{b64}\x1b\\");
    let _ = stdout.flush();
}

pub fn decode_yaml_data_lines(raw: &[String]) -> Vec<String> {
    use base64::Engine;

    let mut out = Vec::with_capacity(raw.len());
    let mut in_data = false;
    for line in raw {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') && !line.starts_with(' ') && !line.starts_with('\t') {
            in_data = trimmed == "data:";
            out.push(line.clone());
            continue;
        }
        if in_data {
            if let Some((key_part, val_part)) = line.split_once(':') {
                let raw_val = val_part.trim();
                let unquoted = raw_val
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .or_else(|| raw_val.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                    .unwrap_or(raw_val);
                if !unquoted.is_empty() {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(unquoted) {
                        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                        match String::from_utf8(bytes) {
                            Ok(text) => {
                                if text.contains('\n') {
                                    out.push(format!("{key_part}: |"));
                                    for sub in text.lines() {
                                        out.push(format!("{indent}  {sub}"));
                                    }
                                } else {
                                    out.push(format!("{key_part}: {text}"));
                                }
                                continue;
                            }
                            Err(e) => {
                                let len = e.into_bytes().len();
                                out.push(format!("{key_part}: <binary: {len} bytes>"));
                                continue;
                            }
                        }
                    }
                }
            }
        }
        out.push(line.clone());
    }
    out
}

fn matches_palette_query(label: &str, query: &str) -> bool {
    let label_lower = label.to_lowercase();
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|t| t.trim_end_matches(':').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return true;
    }
    if tokens.len() >= 2 {
        if let Some((prefix, rest)) = label_lower.split_once(':') {
            return prefix.contains(&tokens[0])
                && tokens[1..].iter().all(|t| rest.contains(t.as_str()));
        }
    }
    let normalized = label_lower.replace(':', "");
    tokens.iter().all(|t| normalized.contains(t.as_str()))
}

fn apply_context_order(
    contexts: &mut [crate::core::kubeconfig::ContextInfo],
    order: &[String],
) {
    if order.is_empty() {
        return;
    }
    contexts.sort_by_key(|c| {
        order
            .iter()
            .position(|name| name == &c.name)
            .unwrap_or(usize::MAX)
    });
}

#[derive(Debug, Clone)]
pub struct ClusterIssue {
    pub context: String,
    pub issue: Issue,
}

pub const DEBUG_IMAGES: [&str; 4] = [
    "busybox:1.37",
    "nicolaka/netshoot:latest",
    "alpine:3.22",
    "ubuntu:24.04",
];

#[derive(Debug, Clone)]
pub enum PendingExternal {
    Shell {
        context: String,
        namespace: String,
        pod: String,
        container: Option<String>,
    },
    Debug {
        context: String,
        namespace: Option<String>,
        name: String,
        is_node: bool,
        image: String,
        target: Option<String>,
    },
    EditYaml {
        context: String,
        kind: KindRef,
        namespace: Option<String>,
        name: String,
        yaml: String,
    },
    #[cfg(unix)]
    Suspend,
}

#[derive(Debug, Clone)]
pub enum ConfirmKind {
    Delete {
        context: String,
        kind: KindRef,
        namespace: Option<String>,
        name: String,
        is_prod: bool,
    },
    Restart {
        context: String,
        kind: KindRef,
        namespace: Option<String>,
        name: String,
    },
    Cordon {
        context: String,
        name: String,
        cordon: bool,
    },
    SuspendCronJob {
        context: String,
        namespace: String,
        name: String,
        suspend: bool,
    },
}

#[derive(Debug, Clone)]
pub enum Modal {
    None,
    Help,
    CommandPalette {
        query: String,
        selected: usize,
    },
    NamespacePicker {
        query: String,
        selected: usize,
    },
    YamlViewer {
        context: String,
        kind: KindRef,
        namespace: Option<String>,
        name: String,
        raw_lines: Vec<String>,
        lines: Vec<String>,
        mode: YamlViewMode,
        fullscreen: bool,
        scroll: usize,
        loading: bool,
        error: Option<String>,
    },
    LogsViewer {
        context: String,
        namespace: String,
        pod: String,
        container: Option<String>,
        containers: Vec<String>,
        lines: Vec<String>,
        fullscreen: bool,
        scroll: usize,
        follow: bool,
        timestamps: bool,
        generation: u64,
        error: Option<String>,
    },
    Confirm {
        kind: ConfirmKind,
        typed: String,
    },
    ScalePrompt {
        context: String,
        kind: KindRef,
        namespace: Option<String>,
        name: String,
        input: String,
    },
    EnvPicker {
        context: String,
        selected: usize,
    },
    DebugPrompt {
        context: String,
        namespace: Option<String>,
        name: String,
        is_node: bool,
        selected_preset: usize,
        custom_image: String,
        target: String,
        editing_target: bool,
    },
    PortForwardPrompt {
        context: String,
        namespace: String,
        kind: String,
        name: String,
        input: String,
        ports: Vec<k8s::AvailablePort>,
        ports_loading: bool,
        selected_port: usize,
        active: Vec<ForwardInfo>,
        selected_active: usize,
        focus_active: bool,
    },
}

#[derive(Debug, Clone)]
pub enum PaletteItem {
    Overview,
    Utilization,
    Kind(KindRef),
    Context(String),
    SetClusterEnv,
    ScanIssues,
    ReloadKubeconfig,
}

impl PaletteItem {
    pub fn label(&self) -> String {
        match self {
            PaletteItem::Overview => "Go to Overview".into(),
            PaletteItem::Utilization => "View: Resource utilization (CPU / Memory)".into(),
            PaletteItem::Kind(k) => {
                if k.group.is_empty() {
                    format!("Resource: {}", k.kind)
                } else {
                    format!("Resource: {} ({})", k.kind, k.group)
                }
            }
            PaletteItem::Context(c) => format!("Cluster: {c}"),
            PaletteItem::SetClusterEnv => "Action: Change cluster environment".into(),
            PaletteItem::ScanIssues => "Action: Scan all resources for issues".into(),
            PaletteItem::ReloadKubeconfig => "Action: Reload kubeconfig".into(),
        }
    }
}

pub struct App {
    pub state: Arc<AppState>,
    pub tx: UnboundedSender<AppEvent>,
    pub should_quit: bool,
    pub pending_external: Option<PendingExternal>,

    pub contexts: Vec<ContextInfo>,
    pub kube_errors: Vec<String>,
    pub selected_contexts: Vec<String>,
    pub sidebar_cursor: usize,
    pub show_sidebar: bool,
    pub focus: Focus,
    pub screen: Screen,
    pub overview_pane: OverviewPane,
    pub overview_cluster_cursor: usize,
    pub overview_issue_cursor: usize,

    pub summaries: HashMap<String, std::result::Result<ClusterSummary, String>>,
    pub summary_loading: HashMap<String, bool>,
    pub issues_by_ctx: BTreeMap<String, Vec<Issue>>,
    pub scan_running: bool,

    pub kinds_by_ctx: HashMap<String, Vec<KindRef>>,
    pub active_kind: KindRef,
    pub namespaces: Vec<String>,
    pub active_namespace: Option<String>,

    pub watch_gen: u64,
    pub watch_handles: Vec<AbortHandle>,
    pub rows_by_ctx: BTreeMap<String, HashMap<String, Row>>,
    pub synced_ctxs: HashMap<String, bool>,
    pub watch_errors: HashMap<String, String>,
    pub table_cursor: usize,
    pub resource_sort: ResourceSort,

    pub filter_active: bool,
    pub filter_text: String,

    pub log_gen: u64,
    pub log_handle: Option<AbortHandle>,

    pub modal: Modal,
    pub toast: Option<(String, bool, std::time::Instant)>,
    pub tick_count: usize,
    pub cli_context: Option<String>,
    pub cli_kubeconfig: Option<String>,
    pub sidebar_width: u16,
    pub overview_split_pct: u16,
    pub dragging: Option<DragTarget>,
    pub term_size: (u16, u16),
    pub utilization_by_ctx: BTreeMap<String, std::result::Result<k8s::ClusterUtilization, String>>,
    pub utilization_loading: HashMap<String, bool>,
    pub utilization_pane: UtilizationPane,
    pub utilization_sort: UtilizationSort,
    pub utilization_node_cursor: usize,
    pub utilization_pod_cursor: usize,
}

impl App {
    pub fn new(
        state: Arc<AppState>,
        tx: UnboundedSender<AppEvent>,
        initial_context: Option<String>,
    ) -> Self {
        Self::new_with_cli(state, tx, initial_context, None)
    }

    pub fn new_with_cli(
        state: Arc<AppState>,
        tx: UnboundedSender<AppEvent>,
        cli_context: Option<String>,
        cli_kubeconfig: Option<String>,
    ) -> Self {
        let report = state.report.read().clone();
        let mut contexts = report.contexts;
        apply_context_order(&mut contexts, &state.settings.read().context_order);
        let kube_errors = report.errors;

        let saved_view = state
            .settings
            .read()
            .tui_view
            .clone()
            .filter(|s| s.cli_context == cli_context && s.cli_kubeconfig == cli_kubeconfig);

        let requested_ctx = cli_context.clone();
        let matched_initial = cli_context
            .clone()
            .filter(|c| contexts.iter().any(|x| &x.name == c));
        let initial_ctx = matched_initial
            .clone()
            .or_else(|| contexts.iter().find(|c| c.is_current).map(|c| c.name.clone()))
            .or_else(|| contexts.first().map(|c| c.name.clone()));

        let selected_contexts = initial_ctx.clone().into_iter().collect::<Vec<_>>();
        let screen = Screen::Overview;

        let active_kind = k8s::builtin_kinds().into_iter().next().unwrap();

        let mut app = Self {
            state,
            tx,
            should_quit: false,
            pending_external: None,
            contexts,
            kube_errors,
            selected_contexts,
            sidebar_cursor: 0,
            show_sidebar: true,
            focus: Focus::Main,
            screen,
            overview_pane: OverviewPane::Clusters,
            overview_cluster_cursor: 0,
            overview_issue_cursor: 0,
            summaries: HashMap::new(),
            summary_loading: HashMap::new(),
            issues_by_ctx: BTreeMap::new(),
            scan_running: false,
            kinds_by_ctx: HashMap::new(),
            active_kind,
            namespaces: Vec::new(),
            active_namespace: None,
            watch_gen: 0,
            watch_handles: Vec::new(),
            rows_by_ctx: BTreeMap::new(),
            synced_ctxs: HashMap::new(),
            watch_errors: HashMap::new(),
            table_cursor: 0,
            resource_sort: ResourceSort::NameAsc,
            filter_active: false,
            filter_text: String::new(),
            log_gen: 0,
            log_handle: None,
            modal: Modal::None,
            toast: None,
            tick_count: 0,
            cli_context,
            cli_kubeconfig,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            overview_split_pct: DEFAULT_OVERVIEW_SPLIT_PCT,
            dragging: None,
            term_size: (120, 40),
            utilization_by_ctx: BTreeMap::new(),
            utilization_loading: HashMap::new(),
            utilization_pane: UtilizationPane::Nodes,
            utilization_sort: UtilizationSort::CpuDesc,
            utilization_node_cursor: 0,
            utilization_pod_cursor: 0,
        };

        app.probe_all_overview();
        if let Some(saved) = saved_view {
            app.apply_saved_view(saved);
        } else if let Some(ctx) = matched_initial {
            app.open_single_cluster(ctx);
        } else {
            if let Some(req) = requested_ctx {
                app.set_toast(format!("Context '{req}' not found"), true);
            }
            if let Some(ctx) = initial_ctx {
                if let Some(pos) = app.contexts.iter().position(|c| c.name == ctx) {
                    app.sidebar_cursor = pos + 1;
                }
                if app.contexts.len() == 1 {
                    app.open_single_cluster(ctx);
                }
            }
        }
        app
    }

    fn apply_saved_view(&mut self, saved: TuiViewState) {
        self.show_sidebar = saved.show_sidebar;
        if let Some(w) = saved.sidebar_width {
            self.sidebar_width = w.clamp(16, 80);
        }
        if let Some(pct) = saved.overview_split_pct {
            self.overview_split_pct = pct.clamp(20, 80);
        }
        self.overview_pane = match saved.overview_pane.as_str() {
            "issues" => OverviewPane::Issues,
            _ => OverviewPane::Clusters,
        };
        self.resource_sort = match saved.resource_sort.as_str() {
            "age_newest" => ResourceSort::AgeNewestFirst,
            "age_oldest" => ResourceSort::AgeOldestFirst,
            _ => ResourceSort::NameAsc,
        };
        self.filter_text = saved.filter_text;
        self.active_namespace = saved.active_namespace;
        if let Some(kind) = saved.active_kind {
            self.active_kind = kind;
        }

        let valid_contexts: Vec<String> = saved
            .selected_contexts
            .into_iter()
            .filter(|c| self.contexts.iter().any(|x| &x.name == c))
            .collect();
        if !valid_contexts.is_empty() {
            self.selected_contexts = valid_contexts;
        }

        if saved.screen == "utilization" && !self.selected_contexts.is_empty() {
            self.open_utilization();
        } else if saved.screen == "resources" && !self.selected_contexts.is_empty() {
            if let Some(first) = self.selected_contexts.first() {
                if let Some(pos) = self.contexts.iter().position(|c| &c.name == first) {
                    self.sidebar_cursor = pos + 1;
                }
            }
            self.screen = Screen::Resources;
            self.focus = Focus::Main;
            self.refresh_metadata_and_watch();
        } else {
            self.screen = Screen::Overview;
            self.sidebar_cursor = 0;
        }
    }

    pub fn save_view_state(&self) {
        let view = TuiViewState {
            cli_context: self.cli_context.clone(),
            cli_kubeconfig: self.cli_kubeconfig.clone(),
            screen: match self.screen {
                Screen::Overview => "overview".into(),
                Screen::Resources => "resources".into(),
                Screen::Utilization => "utilization".into(),
            },
            selected_contexts: self.selected_contexts.clone(),
            active_kind: Some(self.active_kind.clone()),
            active_namespace: self.active_namespace.clone(),
            filter_text: self.filter_text.clone(),
            resource_sort: match self.resource_sort {
                ResourceSort::NameAsc => "name_asc".into(),
                ResourceSort::AgeNewestFirst => "age_newest".into(),
                ResourceSort::AgeOldestFirst => "age_oldest".into(),
            },
            show_sidebar: self.show_sidebar,
            overview_pane: match self.overview_pane {
                OverviewPane::Clusters => "clusters".into(),
                OverviewPane::Issues => "issues".into(),
            },
            sidebar_width: Some(self.sidebar_width),
            overview_split_pct: Some(self.overview_split_pct),
        };
        self.state.settings.write().tui_view = Some(view);
        let _ = self.state.save_settings();
    }

    pub fn set_toast(&mut self, msg: impl Into<String>, is_err: bool) {
        self.toast = Some((msg.into(), is_err, std::time::Instant::now()));
    }

    pub fn context_env(&self, ctx: &str) -> Option<Env> {
        resolve_env(&self.state.settings.read().context_envs, ctx)
    }

    pub fn context_env_choice(&self, ctx: &str) -> EnvChoice {
        self.state
            .settings
            .read()
            .context_envs
            .get(ctx)
            .and_then(|s| EnvChoice::from_setting(s))
            .unwrap_or(EnvChoice::Auto)
    }

    pub fn set_context_env(&mut self, ctx: &str, choice: EnvChoice) {
        {
            let mut settings = self.state.settings.write();
            match choice.as_setting() {
                Some(val) => {
                    settings.context_envs.insert(ctx.to_string(), val.to_string());
                }
                None => {
                    settings.context_envs.remove(ctx);
                }
            }
        }
        if let Err(e) = self.state.save_settings() {
            self.set_toast(format!("Failed to save environment: {e}"), true);
        } else {
            let desc = match self.context_env(ctx) {
                Some(env) if choice == EnvChoice::Auto => format!("Auto ({})", env.tag()),
                None if choice == EnvChoice::Auto => "Auto (None)".into(),
                Some(env) => env.tag().to_string(),
                None => "None".into(),
            };
            self.set_toast(format!("Set '{ctx}' environment to {desc}"), false);
        }
    }

    fn focused_context(&self) -> Option<String> {
        if self.focus == Focus::Sidebar && self.sidebar_cursor > 0 {
            return self
                .contexts
                .get(self.sidebar_cursor - 1)
                .map(|c| c.name.clone());
        }
        if self.screen == Screen::Overview && self.overview_pane == OverviewPane::Clusters {
            return self
                .contexts
                .get(self.overview_cluster_cursor)
                .map(|c| c.name.clone());
        }
        self.selected_contexts
            .first()
            .cloned()
            .or_else(|| self.contexts.first().map(|c| c.name.clone()))
    }

    pub fn open_env_picker(&mut self, ctx: String) {
        let current = self.context_env_choice(&ctx);
        let selected = EnvChoice::ALL
            .iter()
            .position(|&c| c == current)
            .unwrap_or(0);
        self.modal = Modal::EnvPicker {
            context: ctx,
            selected,
        };
    }

    fn move_sidebar_context(&mut self, delta: isize) {
        if self.sidebar_cursor == 0 {
            return;
        }
        let idx = self.sidebar_cursor - 1;
        if let Some(new_idx) = self.move_context_by(idx, delta) {
            self.sidebar_cursor = new_idx + 1;
        }
    }

    fn move_overview_context(&mut self, delta: isize) {
        let idx = self.overview_cluster_cursor;
        if let Some(new_idx) = self.move_context_by(idx, delta) {
            self.overview_cluster_cursor = new_idx;
        }
    }

    fn move_context_by(&mut self, idx: usize, delta: isize) -> Option<usize> {
        let len = self.contexts.len();
        if idx >= len {
            return None;
        }
        let target = idx as isize + delta;
        if target < 0 || target >= len as isize {
            return None;
        }
        let new_idx = target as usize;
        self.contexts.swap(idx, new_idx);
        let order: Vec<String> = self.contexts.iter().map(|c| c.name.clone()).collect();
        self.state.settings.write().context_order = order;
        if let Err(e) = self.state.save_settings() {
            self.set_toast(format!("Failed to save context order: {e}"), true);
        }
        Some(new_idx)
    }

    pub fn probe_all_overview(&mut self) {
        for c in &self.contexts {
            self.summary_loading.insert(c.name.clone(), true);
            k8s::spawn_summary(self.state.clone(), c.name.clone(), self.tx.clone());
            k8s::spawn_issues(self.state.clone(), c.name.clone(), self.tx.clone());
        }
    }

    pub fn open_single_cluster(&mut self, ctx: String) {
        self.selected_contexts = vec![ctx.clone()];
        if let Some(pos) = self.contexts.iter().position(|c| c.name == ctx) {
            self.sidebar_cursor = pos + 1;
        }
        self.screen = Screen::Resources;
        self.focus = Focus::Main;
        self.refresh_metadata_and_watch();
        self.save_view_state();
    }

    pub fn toggle_cluster_multi(&mut self, ctx: String) {
        if let Some(pos) = self.selected_contexts.iter().position(|c| c == &ctx) {
            if self.selected_contexts.len() > 1 {
                self.selected_contexts.remove(pos);
            }
        } else {
            self.selected_contexts.push(ctx);
        }
        self.screen = Screen::Resources;
        self.refresh_metadata_and_watch();
        self.save_view_state();
    }

    pub fn refresh_metadata_and_watch(&mut self) {
        self.namespaces.clear();
        for ctx in &self.selected_contexts {
            k8s::spawn_namespaces(self.state.clone(), ctx.clone(), self.tx.clone());
            if !self.kinds_by_ctx.contains_key(ctx) {
                k8s::spawn_kinds(self.state.clone(), ctx.clone(), self.tx.clone());
            }
        }
        self.restart_watch();
    }

    pub fn restart_watch(&mut self) {
        for h in self.watch_handles.drain(..) {
            h.abort();
        }
        self.watch_gen += 1;
        self.rows_by_ctx.clear();
        self.synced_ctxs.clear();
        self.watch_errors.clear();
        self.table_cursor = 0;

        let generation = self.watch_gen;
        for ctx in &self.selected_contexts {
            self.synced_ctxs.insert(ctx.clone(), false);
            let h = k8s::spawn_watch(
                self.state.clone(),
                generation,
                ctx.clone(),
                self.active_kind.clone(),
                self.active_namespace.clone(),
                self.tx.clone(),
            );
            self.watch_handles.push(h);
        }
    }

    pub fn all_issues(&self) -> Vec<ClusterIssue> {
        let mut out = Vec::new();
        for (ctx, list) in &self.issues_by_ctx {
            for issue in list {
                out.push(ClusterIssue {
                    context: ctx.clone(),
                    issue: issue.clone(),
                });
            }
        }
        out.sort_by(|a, b| {
            a.issue
                .severity
                .cmp(&b.issue.severity)
                .then_with(|| a.context.cmp(&b.context))
                .then_with(|| a.issue.kind.cmp(&b.issue.kind))
                .then_with(|| a.issue.name.cmp(&b.issue.name))
        });
        out
    }

    pub fn filtered_rows(&self) -> Vec<TableRow> {
        let mut all = Vec::new();
        for (ctx, map) in &self.rows_by_ctx {
            if !self.selected_contexts.contains(ctx) {
                continue;
            }
            for row in map.values() {
                all.push(TableRow {
                    context: ctx.clone(),
                    row: row.clone(),
                });
            }
        }

        let q = self.filter_text.trim().to_lowercase();
        if !q.is_empty() {
            let terms: Vec<&str> = q.split_whitespace().collect();
            all.retain(|tr| {
                terms.iter().all(|term| match *term {
                    "status:bad" => matches!(tr.row.health, Health::Error | Health::Warn),
                    "status:error" => tr.row.health == Health::Error,
                    "status:warn" => tr.row.health == Health::Warn,
                    "status:ok" => tr.row.health == Health::Ok,
                    word => {
                        tr.context.to_lowercase().contains(word)
                            || tr.row.name.to_lowercase().contains(word)
                            || tr
                                .row
                                .namespace
                                .as_deref()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(word)
                            || tr.row.cells.iter().any(|c| c.to_lowercase().contains(word))
                    }
                })
            });
        }

        all.sort_by(|a, b| {
            let primary = match self.resource_sort {
                ResourceSort::NameAsc => std::cmp::Ordering::Equal,
                ResourceSort::AgeNewestFirst => match (&a.row.created, &b.row.created) {
                    (Some(ca), Some(cb)) => cb.cmp(ca),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
                ResourceSort::AgeOldestFirst => match (&a.row.created, &b.row.created) {
                    (Some(ca), Some(cb)) => ca.cmp(cb),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
            };
            primary
                .then_with(|| a.row.name.cmp(&b.row.name))
                .then_with(|| a.context.cmp(&b.context))
                .then_with(|| a.row.namespace.cmp(&b.row.namespace))
        });
        all
    }

    pub fn available_kinds(&self) -> Vec<KindRef> {
        let mut kinds = k8s::builtin_kinds();
        let mut seen: std::collections::HashSet<(String, String)> =
            kinds.iter().map(|k| (k.group.clone(), k.kind.clone())).collect();
        for ctx in &self.selected_contexts {
            if let Some(discovered) = self.kinds_by_ctx.get(ctx) {
                for k in discovered {
                    if seen.insert((k.group.clone(), k.kind.clone())) {
                        kinds.push(k.clone());
                    }
                }
            }
        }
        kinds
    }

    pub fn open_utilization(&mut self) {
        self.screen = Screen::Utilization;
        self.focus = Focus::Main;
        self.refresh_utilization();
    }

    pub fn refresh_utilization(&mut self) {
        let targets: Vec<String> = if self.selected_contexts.is_empty() {
            self.contexts.first().map(|c| vec![c.name.clone()]).unwrap_or_default()
        } else {
            self.selected_contexts.clone()
        };
        for ctx in targets {
            self.utilization_loading.insert(ctx.clone(), true);
            k8s::spawn_utilization(self.state.clone(), ctx, self.tx.clone());
        }
    }

    pub fn utilization_nodes(&self) -> Vec<(String, k8s::NodeUsageEntry)> {
        let q = self.filter_text.trim().to_lowercase();
        let mut out = Vec::new();
        for ctx in &self.selected_contexts {
            if let Some(Ok(u)) = self.utilization_by_ctx.get(ctx) {
                for n in &u.nodes {
                    if q.is_empty()
                        || n.name.to_lowercase().contains(&q)
                        || ctx.to_lowercase().contains(&q)
                    {
                        out.push((ctx.clone(), n.clone()));
                    }
                }
            }
        }
        out.sort_by(|a, b| match self.utilization_sort {
            UtilizationSort::CpuDesc => b
                .1
                .cpu_used
                .unwrap_or(b.1.cpu_req)
                .total_cmp(&a.1.cpu_used.unwrap_or(a.1.cpu_req)),
            UtilizationSort::MemDesc => b
                .1
                .mem_used
                .unwrap_or(b.1.mem_req)
                .total_cmp(&a.1.mem_used.unwrap_or(a.1.mem_req)),
        });
        out
    }

    pub fn utilization_pods(&self) -> Vec<(String, k8s::PodUsageEntry)> {
        let q = self.filter_text.trim().to_lowercase();
        let mut out = Vec::new();
        for ctx in &self.selected_contexts {
            if let Some(Ok(u)) = self.utilization_by_ctx.get(ctx) {
                for p in &u.pods {
                    if q.is_empty()
                        || p.name.to_lowercase().contains(&q)
                        || p.namespace.to_lowercase().contains(&q)
                        || ctx.to_lowercase().contains(&q)
                        || p.node
                            .as_deref()
                            .is_some_and(|n| n.to_lowercase().contains(&q))
                    {
                        out.push((ctx.clone(), p.clone()));
                    }
                }
            }
        }
        out.sort_by(|a, b| match self.utilization_sort {
            UtilizationSort::CpuDesc => b
                .1
                .cpu_used
                .unwrap_or(b.1.cpu_req)
                .total_cmp(&a.1.cpu_used.unwrap_or(a.1.cpu_req)),
            UtilizationSort::MemDesc => b
                .1
                .mem_used
                .unwrap_or(b.1.mem_req)
                .total_cmp(&a.1.mem_used.unwrap_or(a.1.mem_req)),
        });
        out
    }

    pub fn palette_items(&self, query: &str) -> Vec<PaletteItem> {
        let mut items = Vec::new();
        items.push(PaletteItem::Overview);
        items.push(PaletteItem::Utilization);
        for k in self.available_kinds() {
            items.push(PaletteItem::Kind(k));
        }
        for c in &self.contexts {
            items.push(PaletteItem::Context(c.name.clone()));
        }
        items.push(PaletteItem::SetClusterEnv);
        items.push(PaletteItem::ScanIssues);
        items.push(PaletteItem::ReloadKubeconfig);

        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            items.retain(|i| matches_palette_query(&i.label(), &q));
        }
        items
    }

    pub fn filtered_namespaces(&self, query: &str) -> Vec<Option<String>> {
        let mut out = vec![None];
        for ns in &self.namespaces {
            out.push(Some(ns.clone()));
        }
        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            out.retain(|item| match item {
                None => "all namespaces".contains(&q),
                Some(ns) => ns.to_lowercase().contains(&q),
            });
        }
        out
    }

    pub fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::SummaryLoaded { context, result } => {
                self.summary_loading.insert(context.clone(), false);
                self.summaries.insert(context, result);
            }
            AppEvent::UtilizationLoaded { context, result } => {
                self.utilization_loading.insert(context.clone(), false);
                self.utilization_by_ctx.insert(context, result);
            }
            AppEvent::PortForwardStarted(res) => match res {
                Ok(info) => {
                    self.set_toast(
                        format!(
                            "Forwarding localhost:{} -> {}/{}:{}",
                            info.local_port, info.namespace, info.target, info.remote_port
                        ),
                        false,
                    );
                }
                Err(e) => self.set_toast(format!("Port-forward failed: {e}"), true),
            },
            AppEvent::PortsLoaded {
                context,
                namespace,
                kind,
                name,
                result,
            } => {
                if let Modal::PortForwardPrompt {
                    context: mc,
                    namespace: mns,
                    kind: mk,
                    name: mn,
                    ports,
                    ports_loading,
                    selected_port,
                    ..
                } = &mut self.modal
                {
                    if *mc == context && *mns == namespace && *mk == kind && *mn == name {
                        *ports_loading = false;
                        match result {
                            Ok(list) => {
                                *ports = list;
                                *selected_port = 0;
                            }
                            Err(e) => self.set_toast(format!("Failed to load ports: {e}"), true),
                        }
                    }
                }
            }
            AppEvent::IssuesLoaded { context, result } => {
                if let Ok(list) = result {
                    self.issues_by_ctx.insert(context, list);
                }
            }
            AppEvent::ScanFinished { context, result } => {
                self.scan_running = false;
                match result {
                    Ok(Scan { items, kinds, skipped }) => {
                        let count = items.len();
                        self.issues_by_ctx.insert(context.clone(), items);
                        let skip_note = if skipped.is_empty() {
                            String::new()
                        } else {
                            format!(" ({} skipped)", skipped.len())
                        };
                        self.set_toast(
                            format!("Scanned {kinds} kinds in {context}: {count} issue(s){skip_note}"),
                            false,
                        );
                    }
                    Err(e) => self.set_toast(format!("Scan failed: {e}"), true),
                }
            }
            AppEvent::NamespacesLoaded { result } => {
                if let Ok(mut list) = result {
                    let mut set: std::collections::BTreeSet<String> =
                        self.namespaces.iter().cloned().collect();
                    set.extend(list.drain(..));
                    self.namespaces = set.into_iter().collect();
                }
            }
            AppEvent::KindsLoaded { context, result } => {
                if let Ok(list) = result {
                    self.kinds_by_ctx.insert(context, list);
                }
            }
            AppEvent::Watch(upd) => match upd {
                WatchUpdate::Upsert { generation, context, rows } => {
                    if generation != self.watch_gen {
                        return;
                    }
                    let map = self.rows_by_ctx.entry(context).or_default();
                    for r in rows {
                        map.insert(r.uid.clone(), r);
                    }
                }
                WatchUpdate::Delete { generation, context, uids } => {
                    if generation != self.watch_gen {
                        return;
                    }
                    if let Some(map) = self.rows_by_ctx.get_mut(&context) {
                        for u in uids {
                            map.remove(&u);
                        }
                    }
                }
                WatchUpdate::Synced { generation, context, uids } => {
                    if generation != self.watch_gen {
                        return;
                    }
                    let keep: std::collections::HashSet<String> = uids.into_iter().collect();
                    if let Some(map) = self.rows_by_ctx.get_mut(&context) {
                        map.retain(|uid, _| keep.contains(uid));
                    }
                    self.synced_ctxs.insert(context.clone(), true);
                    self.watch_errors.remove(&context);
                }
                WatchUpdate::Error { generation, context, message } => {
                    if generation != self.watch_gen {
                        return;
                    }
                    self.watch_errors.insert(context, message);
                }
            },
            AppEvent::YamlLoaded {
                context,
                name,
                namespace,
                kind,
                result,
            } => {
                if let Modal::YamlViewer {
                    context: mc,
                    name: mn,
                    namespace: mns,
                    kind: mk,
                    raw_lines,
                    lines,
                    mode,
                    loading,
                    error,
                    ..
                } = &mut self.modal
                {
                    if *mc == context && *mn == name && *mns == namespace && mk.kind == kind.kind {
                        *loading = false;
                        match result {
                            Ok(yaml) => {
                                *raw_lines = yaml.lines().map(ToString::to_string).collect();
                                *lines = yaml_lines_for_mode(raw_lines, *mode);
                                *error = None;
                            }
                            Err(e) => *error = Some(e),
                        }
                    }
                }
            }
            AppEvent::ContainersLoaded {
                context,
                namespace,
                pod,
                containers,
            } => {
                if let Modal::LogsViewer {
                    context: mc,
                    namespace: mns,
                    pod: mp,
                    containers: mc_list,
                    container,
                    ..
                } = &mut self.modal
                {
                    if *mc == context && *mns == namespace && *mp == pod {
                        *mc_list = containers;
                        if container.is_none() {
                            *container = mc_list.first().cloned();
                        }
                    }
                }
            }
            AppEvent::LogLine { generation, line } => {
                if let Modal::LogsViewer {
                    generation: mg,
                    lines,
                    scroll,
                    follow,
                    ..
                } = &mut self.modal
                {
                    if *mg == generation {
                        lines.push(line);
                        if lines.len() > 5000 {
                            lines.drain(..lines.len() - 5000);
                        }
                        if *follow {
                            *scroll = lines.len().saturating_sub(1);
                        }
                    }
                }
            }
            AppEvent::LogError { generation, message } => {
                if let Modal::LogsViewer {
                    generation: mg,
                    error,
                    ..
                } = &mut self.modal
                {
                    if *mg == generation {
                        *error = Some(message);
                    }
                }
            }
            AppEvent::ActionDone { message, refresh_yaml } => match message {
                Ok(msg) => {
                    self.set_toast(msg, false);
                    if refresh_yaml {
                        if let Modal::YamlViewer {
                            context,
                            kind,
                            namespace,
                            name,
                            loading,
                            ..
                        } = &mut self.modal
                        {
                            *loading = true;
                            k8s::spawn_yaml(
                                self.state.clone(),
                                context.clone(),
                                kind.clone(),
                                namespace.clone(),
                                name.clone(),
                                self.tx.clone(),
                            );
                        }
                    }
                }
                Err(e) => self.set_toast(e, true),
            },
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(self.modal, Modal::None) {
            return;
        }
        let (term_w, term_h) = self.term_size;
        let body_top: u16 = 3;
        let body_bottom = term_h.saturating_sub(2);
        let body_h = body_bottom.saturating_sub(body_top).max(1);
        let max_sidebar = term_w.saturating_sub(20).max(16);

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if mouse.row < body_top || mouse.row >= body_bottom {
                    return;
                }
                if self.show_sidebar {
                    let border_x = self.sidebar_width.clamp(16, max_sidebar);
                    if mouse.column >= border_x.saturating_sub(1) && mouse.column <= border_x + 1 {
                        self.dragging = Some(DragTarget::Sidebar);
                        return;
                    }
                    if mouse.column < border_x.saturating_sub(1) {
                        self.focus = Focus::Sidebar;
                        return;
                    }
                }
                self.focus = Focus::Main;
                if self.screen == Screen::Overview {
                    let split_y = body_top
                        + ((body_h as u32 * self.overview_split_pct as u32) / 100) as u16;
                    if mouse.row >= split_y.saturating_sub(1) && mouse.row <= split_y + 1 {
                        self.dragging = Some(DragTarget::OverviewSplit);
                    } else if mouse.row < split_y.saturating_sub(1) {
                        self.overview_pane = OverviewPane::Clusters;
                    } else {
                        self.overview_pane = OverviewPane::Issues;
                    }
                } else if self.screen == Screen::Utilization {
                    let split_y = body_top + ((body_h as u32 * 40) / 100) as u16;
                    if mouse.row < split_y {
                        self.utilization_pane = UtilizationPane::Nodes;
                    } else {
                        self.utilization_pane = UtilizationPane::Pods;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.dragging {
                Some(DragTarget::Sidebar) => {
                    self.sidebar_width = mouse.column.saturating_add(1).clamp(16, max_sidebar);
                }
                Some(DragTarget::OverviewSplit) => {
                    let rel_y = mouse.row.saturating_sub(body_top);
                    let pct = ((rel_y as u32 * 100) / body_h as u32).clamp(20, 80) as u16;
                    self.overview_split_pct = pct;
                }
                None => {}
            },
            MouseEventKind::Up(MouseButton::Left) => {
                if self.dragging.take().is_some() {
                    self.save_view_state();
                }
            }
            _ => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.save_view_state();
            self.should_quit = true;
            return;
        }
        #[cfg(unix)]
        if key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.pending_external = Some(PendingExternal::Suspend);
            return;
        }

        if !matches!(self.modal, Modal::None) {
            self.handle_modal_key(key);
            return;
        }

        if self.filter_active {
            match key.code {
                KeyCode::Esc => {
                    self.filter_active = false;
                    self.save_view_state();
                }
                KeyCode::Enter => {
                    self.filter_active = false;
                    self.save_view_state();
                }
                KeyCode::Backspace => {
                    self.filter_text.pop();
                    self.table_cursor = 0;
                    self.utilization_node_cursor = 0;
                    self.utilization_pod_cursor = 0;
                }
                KeyCode::Char(ch) => {
                    self.filter_text.push(ch);
                    self.table_cursor = 0;
                    self.utilization_node_cursor = 0;
                    self.utilization_pod_cursor = 0;
                }
                _ => {}
            }
            return;
        }

        // Global shortcuts
        match key.code {
            KeyCode::Char('q') => {
                self.save_view_state();
                self.should_quit = true;
                return;
            }
            KeyCode::Char('?') => {
                self.modal = Modal::Help;
                return;
            }
            KeyCode::Char('b') => {
                self.show_sidebar = !self.show_sidebar;
                if !self.show_sidebar {
                    self.focus = Focus::Main;
                }
                self.save_view_state();
                return;
            }
            KeyCode::Tab => {
                if self.show_sidebar {
                    self.focus = match self.focus {
                        Focus::Sidebar => Focus::Main,
                        Focus::Main => Focus::Sidebar,
                    };
                }
                return;
            }
            KeyCode::Char(':') => {
                self.modal = Modal::CommandPalette {
                    query: String::new(),
                    selected: 0,
                };
                return;
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.modal = Modal::CommandPalette {
                    query: String::new(),
                    selected: 0,
                };
                return;
            }
            KeyCode::Char('0') => {
                self.sidebar_cursor = 0;
                self.screen = Screen::Overview;
                self.focus = Focus::Main;
                return;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                if let Some(ctx) = self.contexts.get(idx).map(|x| x.name.clone()) {
                    self.open_single_cluster(ctx);
                }
                return;
            }
            KeyCode::Char('u') => {
                self.open_utilization();
                return;
            }
            KeyCode::Char('R') => {
                let report = self.state.reload();
                let mut contexts = report.contexts;
                apply_context_order(&mut contexts, &self.state.settings.read().context_order);
                self.contexts = contexts;
                self.kube_errors = report.errors;
                self.probe_all_overview();
                if self.screen == Screen::Resources {
                    self.refresh_metadata_and_watch();
                }
                self.set_toast("Reloaded kubeconfig", false);
                return;
            }
            _ => {}
        }

        if self.focus == Focus::Sidebar {
            self.handle_sidebar_key(key);
            return;
        }

        match self.screen {
            Screen::Overview => self.handle_overview_key(key),
            Screen::Resources => self.handle_resources_key(key),
            Screen::Utilization => self.handle_utilization_key(key),
        }
    }

    fn handle_utilization_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                if self.show_sidebar {
                    self.focus = Focus::Sidebar;
                }
            }
            KeyCode::Esc => {
                if !self.filter_text.is_empty() {
                    self.filter_text.clear();
                    self.utilization_node_cursor = 0;
                    self.utilization_pod_cursor = 0;
                    self.save_view_state();
                } else {
                    self.screen = Screen::Overview;
                }
            }
            KeyCode::Char('/') => {
                self.filter_active = true;
            }
            KeyCode::Char('o') | KeyCode::BackTab => {
                self.utilization_pane = match self.utilization_pane {
                    UtilizationPane::Nodes => UtilizationPane::Pods,
                    UtilizationPane::Pods => UtilizationPane::Nodes,
                };
            }
            KeyCode::Char('c') => {
                self.utilization_sort = UtilizationSort::CpuDesc;
                self.utilization_node_cursor = 0;
                self.utilization_pod_cursor = 0;
                self.set_toast("Sorted utilization by CPU", false);
            }
            KeyCode::Char('m') => {
                self.utilization_sort = UtilizationSort::MemDesc;
                self.utilization_node_cursor = 0;
                self.utilization_pod_cursor = 0;
                self.set_toast("Sorted utilization by Memory", false);
            }
            KeyCode::Char('r') => {
                self.refresh_utilization();
                self.set_toast("Refreshing utilization metrics...", false);
            }
            KeyCode::Up | KeyCode::Char('k') => match self.utilization_pane {
                UtilizationPane::Nodes => {
                    self.utilization_node_cursor = self.utilization_node_cursor.saturating_sub(1);
                }
                UtilizationPane::Pods => {
                    self.utilization_pod_cursor = self.utilization_pod_cursor.saturating_sub(1);
                }
            },
            KeyCode::Down | KeyCode::Char('j') => match self.utilization_pane {
                UtilizationPane::Nodes => {
                    let len = self.utilization_nodes().len();
                    if self.utilization_node_cursor + 1 < len {
                        self.utilization_node_cursor += 1;
                    }
                }
                UtilizationPane::Pods => {
                    let len = self.utilization_pods().len();
                    if self.utilization_pod_cursor + 1 < len {
                        self.utilization_pod_cursor += 1;
                    }
                }
            },
            KeyCode::Char('g') => match self.utilization_pane {
                UtilizationPane::Nodes => self.utilization_node_cursor = 0,
                UtilizationPane::Pods => self.utilization_pod_cursor = 0,
            },
            KeyCode::Char('G') => match self.utilization_pane {
                UtilizationPane::Nodes => {
                    self.utilization_node_cursor = self.utilization_nodes().len().saturating_sub(1);
                }
                UtilizationPane::Pods => {
                    self.utilization_pod_cursor = self.utilization_pods().len().saturating_sub(1);
                }
            },
            KeyCode::Enter => {
                if self.utilization_pane == UtilizationPane::Pods {
                    if let Some((ctx, pod)) = self.utilization_pods().get(self.utilization_pod_cursor) {
                        if let Some(k) = self.available_kinds().into_iter().find(|k| k.kind == "Pod") {
                            self.active_kind = k;
                        }
                        self.active_namespace = Some(pod.namespace.clone());
                        self.filter_text = pod.name.clone();
                        self.open_single_cluster(ctx.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) {
        let total = 1 + self.contexts.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.move_sidebar_context(-1);
                } else {
                    self.sidebar_cursor = self.sidebar_cursor.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.move_sidebar_context(1);
                } else if self.sidebar_cursor + 1 < total {
                    self.sidebar_cursor += 1;
                }
            }
            KeyCode::Char('K') => {
                self.move_sidebar_context(-1);
            }
            KeyCode::Char('J') => {
                self.move_sidebar_context(1);
            }
            KeyCode::Enter => {
                if self.sidebar_cursor == 0 {
                    self.screen = Screen::Overview;
                    self.focus = Focus::Main;
                } else if let Some(ctx) = self.contexts.get(self.sidebar_cursor - 1).map(|c| c.name.clone()) {
                    self.open_single_cluster(ctx);
                }
            }
            KeyCode::Char(' ') | KeyCode::Char('m') => {
                if self.sidebar_cursor > 0 {
                    if let Some(ctx) = self.contexts.get(self.sidebar_cursor - 1).map(|c| c.name.clone()) {
                        self.toggle_cluster_multi(ctx);
                    }
                }
            }
            KeyCode::Char('e') => {
                if self.sidebar_cursor > 0 {
                    if let Some(ctx) = self.contexts.get(self.sidebar_cursor - 1).map(|c| c.name.clone()) {
                        self.open_env_picker(ctx);
                    }
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.focus = Focus::Main;
            }
            _ => {}
        }
    }

    fn handle_overview_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                if self.show_sidebar {
                    self.focus = Focus::Sidebar;
                }
            }
            KeyCode::Char('o') | KeyCode::BackTab => {
                self.overview_pane = match self.overview_pane {
                    OverviewPane::Clusters => OverviewPane::Issues,
                    OverviewPane::Issues => OverviewPane::Clusters,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => match self.overview_pane {
                OverviewPane::Clusters => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        self.move_overview_context(-1);
                    } else {
                        self.overview_cluster_cursor = self.overview_cluster_cursor.saturating_sub(1);
                    }
                }
                OverviewPane::Issues => {
                    self.overview_issue_cursor = self.overview_issue_cursor.saturating_sub(1);
                }
            },
            KeyCode::Down | KeyCode::Char('j') => match self.overview_pane {
                OverviewPane::Clusters => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        self.move_overview_context(1);
                    } else if self.overview_cluster_cursor + 1 < self.contexts.len() {
                        self.overview_cluster_cursor += 1;
                    }
                }
                OverviewPane::Issues => {
                    let len = self.all_issues().len();
                    if self.overview_issue_cursor + 1 < len {
                        self.overview_issue_cursor += 1;
                    }
                }
            },
            KeyCode::Char('K') => {
                if self.overview_pane == OverviewPane::Clusters {
                    self.move_overview_context(-1);
                }
            }
            KeyCode::Char('J') => {
                if self.overview_pane == OverviewPane::Clusters {
                    self.move_overview_context(1);
                }
            }
            KeyCode::Enter => match self.overview_pane {
                OverviewPane::Clusters => {
                    if let Some(ctx) = self.contexts.get(self.overview_cluster_cursor).map(|c| c.name.clone()) {
                        self.open_single_cluster(ctx);
                    }
                }
                OverviewPane::Issues => {
                    let issues = self.all_issues();
                    if let Some(ci) = issues.get(self.overview_issue_cursor) {
                        let ctx = ci.context.clone();
                        let kind_name = ci.issue.kind.clone();
                        let group = ci.issue.group.clone();
                        let name = ci.issue.name.clone();
                        let ns = ci.issue.namespace.clone();
                        if let Some(k) = self
                            .available_kinds()
                            .into_iter()
                            .find(|k| k.kind == kind_name && k.group == group)
                        {
                            self.active_kind = k;
                        }
                        self.active_namespace = ns;
                        self.filter_text = name;
                        self.open_single_cluster(ctx);
                    }
                }
            },
            KeyCode::Char(' ') | KeyCode::Char('m') => {
                if self.overview_pane == OverviewPane::Clusters {
                    if let Some(ctx) = self.contexts.get(self.overview_cluster_cursor).map(|c| c.name.clone()) {
                        self.toggle_cluster_multi(ctx);
                    }
                }
            }
            KeyCode::Char('e') => {
                if self.overview_pane == OverviewPane::Clusters {
                    if let Some(ctx) = self.contexts.get(self.overview_cluster_cursor).map(|c| c.name.clone()) {
                        self.open_env_picker(ctx);
                    }
                }
            }
            KeyCode::Char('s') => {
                if !self.scan_running {
                    if let Some(ctx) = self.contexts.get(self.overview_cluster_cursor).map(|c| c.name.clone()) {
                        self.scan_running = true;
                        self.set_toast(format!("Scanning all resources in {ctx}..."), false);
                        k8s::spawn_scan(self.state.clone(), ctx, self.tx.clone());
                    }
                }
            }
            KeyCode::Char('r') => {
                self.probe_all_overview();
                self.set_toast("Refreshing overview...", false);
            }
            _ => {}
        }
    }

    fn handle_resources_key(&mut self, key: KeyEvent) {
        let rows = self.filtered_rows();
        let count = rows.len();
        if self.table_cursor >= count && count > 0 {
            self.table_cursor = count - 1;
        }

        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                if self.show_sidebar {
                    self.focus = Focus::Sidebar;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.table_cursor = self.table_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.table_cursor + 1 < count {
                    self.table_cursor += 1;
                }
            }
            KeyCode::PageUp => {
                self.table_cursor = self.table_cursor.saturating_sub(15);
            }
            KeyCode::PageDown => {
                if count > 0 {
                    self.table_cursor = (self.table_cursor + 15).min(count - 1);
                }
            }
            KeyCode::Char('g') => {
                self.table_cursor = 0;
            }
            KeyCode::Char('G') => {
                if count > 0 {
                    self.table_cursor = count - 1;
                }
            }
            KeyCode::Char('/') => {
                self.filter_active = true;
            }
            KeyCode::Esc => {
                if !self.filter_text.is_empty() {
                    self.filter_text.clear();
                    self.table_cursor = 0;
                }
            }
            KeyCode::Char('n') => {
                self.modal = Modal::NamespacePicker {
                    query: String::new(),
                    selected: 0,
                };
            }
            KeyCode::Char('[') => {
                self.cycle_kind(-1);
            }
            KeyCode::Char(']') => {
                self.cycle_kind(1);
            }
            KeyCode::Char('A') => {
                self.resource_sort = match self.resource_sort {
                    ResourceSort::NameAsc => ResourceSort::AgeNewestFirst,
                    ResourceSort::AgeNewestFirst => ResourceSort::AgeOldestFirst,
                    ResourceSort::AgeOldestFirst => ResourceSort::NameAsc,
                };
                self.table_cursor = 0;
                let label = match self.resource_sort {
                    ResourceSort::NameAsc => "Sorted by Name",
                    ResourceSort::AgeNewestFirst => "Sorted by Age (newest first)",
                    ResourceSort::AgeOldestFirst => "Sorted by Age (oldest first)",
                };
                self.set_toast(label, false);
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                if let Some(tr) = rows.get(self.table_cursor) {
                    self.open_yaml_viewer(tr.context.clone(), tr.row.namespace.clone(), tr.row.name.clone(), YamlViewMode::Raw);
                }
            }
            KeyCode::Char('x') => {
                if let Some(tr) = rows.get(self.table_cursor) {
                    self.open_yaml_viewer(tr.context.clone(), tr.row.namespace.clone(), tr.row.name.clone(), YamlViewMode::Decoded);
                }
            }
            KeyCode::Char('i') => {
                if let Some(tr) = rows.get(self.table_cursor) {
                    self.open_yaml_viewer(tr.context.clone(), tr.row.namespace.clone(), tr.row.name.clone(), YamlViewMode::Interpreted);
                }
            }
            KeyCode::Char('l') => {
                if self.active_kind.kind == "Pod" {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        if let Some(ns) = tr.row.namespace.clone() {
                            self.open_logs_viewer(tr.context.clone(), ns, tr.row.name.clone());
                        }
                    }
                } else {
                    self.set_toast("Logs are available for Pods", true);
                }
            }
            KeyCode::Char('s') => {
                if self.active_kind.kind == "Pod" {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        if let Some(ns) = tr.row.namespace.clone() {
                            self.pending_external = Some(PendingExternal::Shell {
                                context: tr.context.clone(),
                                namespace: ns,
                                pod: tr.row.name.clone(),
                                container: None,
                            });
                        }
                    }
                } else {
                    self.set_toast("Shell is available for Pods", true);
                }
            }
            KeyCode::Char('D') => {
                if self.active_kind.kind == "Pod" {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        if let Some(ns) = tr.row.namespace.clone() {
                            self.modal = Modal::DebugPrompt {
                                context: tr.context.clone(),
                                namespace: Some(ns),
                                name: tr.row.name.clone(),
                                is_node: false,
                                selected_preset: 0,
                                custom_image: String::new(),
                                target: String::new(),
                                editing_target: false,
                            };
                        }
                    }
                } else if self.active_kind.kind == "Node" {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        self.modal = Modal::DebugPrompt {
                            context: tr.context.clone(),
                            namespace: None,
                            name: tr.row.name.clone(),
                            is_node: true,
                            selected_preset: 0,
                            custom_image: String::new(),
                            target: String::new(),
                            editing_target: false,
                        };
                    }
                } else {
                    self.set_toast("Debug container is available for Pods and Nodes", true);
                }
            }
            KeyCode::Char('F') => {
                if matches!(self.active_kind.kind.as_str(), "Pod" | "Service") {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        if let Some(ns) = tr.row.namespace.clone() {
                            let active = k8s::list_port_forwards(&self.state);
                            let ctx = tr.context.clone();
                            let kind = self.active_kind.kind.clone();
                            let name = tr.row.name.clone();
                            self.modal = Modal::PortForwardPrompt {
                                context: ctx.clone(),
                                namespace: ns.clone(),
                                kind: kind.clone(),
                                name: name.clone(),
                                input: String::new(),
                                ports: Vec::new(),
                                ports_loading: true,
                                selected_port: 0,
                                active,
                                selected_active: 0,
                                focus_active: false,
                            };
                            k8s::spawn_available_ports(
                                self.state.clone(),
                                ctx,
                                ns,
                                kind,
                                name,
                                self.tx.clone(),
                            );
                        }
                    }
                } else {
                    self.set_toast("Port-forward is available for Pods and Services", true);
                }
            }
            KeyCode::Char('E') => {
                if let Some(tr) = rows.get(self.table_cursor) {
                    let state = self.state.clone();
                    let ctx = tr.context.clone();
                    let kind = self.active_kind.clone();
                    let ns = tr.row.namespace.clone();
                    let name = tr.row.name.clone();
                    // Load YAML synchronously on a blocking task or reuse existing? Let's spawn via tokio block_in_place in main or load via k8s.
                    // Easiest: fetch via tokio::runtime::Handle::current().block_on in main? Better: fetch YAML first or block_on in main when pending_external?
                    // Actually, let's fetch YAML right now using tokio::task::block_in_place in main, or store empty yaml and fetch in main!
                    let _ = state;
                    self.pending_external = Some(PendingExternal::EditYaml {
                        context: ctx,
                        kind,
                        namespace: ns,
                        name,
                        yaml: String::new(),
                    });
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(tr) = rows.get(self.table_cursor) {
                    let is_prod = self.context_env(&tr.context) == Some(Env::Prod);
                    self.modal = Modal::Confirm {
                        kind: ConfirmKind::Delete {
                            context: tr.context.clone(),
                            kind: self.active_kind.clone(),
                            namespace: tr.row.namespace.clone(),
                            name: tr.row.name.clone(),
                            is_prod,
                        },
                        typed: String::new(),
                    };
                }
            }
            KeyCode::Char('r') => {
                if matches!(
                    self.active_kind.kind.as_str(),
                    "Deployment" | "StatefulSet" | "DaemonSet"
                ) {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        self.modal = Modal::Confirm {
                            kind: ConfirmKind::Restart {
                                context: tr.context.clone(),
                                kind: self.active_kind.clone(),
                                namespace: tr.row.namespace.clone(),
                                name: tr.row.name.clone(),
                            },
                            typed: String::new(),
                        };
                    }
                }
            }
            KeyCode::Char('S') => {
                if matches!(
                    self.active_kind.kind.as_str(),
                    "Deployment" | "StatefulSet" | "ReplicaSet"
                ) {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        self.modal = Modal::ScalePrompt {
                            context: tr.context.clone(),
                            kind: self.active_kind.clone(),
                            namespace: tr.row.namespace.clone(),
                            name: tr.row.name.clone(),
                            input: String::new(),
                        };
                    }
                }
            }
            KeyCode::Char('c') => {
                if self.active_kind.kind == "Node" {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        let cordoned = tr
                            .row
                            .cells
                            .first()
                            .is_some_and(|s| s.contains("SchedulingDisabled"));
                        self.modal = Modal::Confirm {
                            kind: ConfirmKind::Cordon {
                                context: tr.context.clone(),
                                name: tr.row.name.clone(),
                                cordon: !cordoned,
                            },
                            typed: String::new(),
                        };
                    }
                }
            }
            KeyCode::Char('p') => {
                if self.active_kind.kind == "CronJob" {
                    if let Some(tr) = rows.get(self.table_cursor) {
                        if let Some(ns) = tr.row.namespace.clone() {
                            let suspended = tr.row.cells.get(1).is_some_and(|s| s == "True");
                            self.modal = Modal::Confirm {
                                kind: ConfirmKind::SuspendCronJob {
                                    context: tr.context.clone(),
                                    namespace: ns,
                                    name: tr.row.name.clone(),
                                    suspend: !suspended,
                                },
                                typed: String::new(),
                            };
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn cycle_kind(&mut self, delta: isize) {
        let builtins = k8s::builtin_kinds();
        let pos = builtins
            .iter()
            .position(|k| k.kind == self.active_kind.kind && k.group == self.active_kind.group)
            .unwrap_or(0) as isize;
        let len = builtins.len() as isize;
        let next = ((pos + delta).rem_euclid(len)) as usize;
        self.active_kind = builtins[next].clone();
        self.restart_watch();
    }

    fn open_yaml_viewer(
        &mut self,
        context: String,
        namespace: Option<String>,
        name: String,
        mode: YamlViewMode,
    ) {
        let kind = self.active_kind.clone();
        self.modal = Modal::YamlViewer {
            context: context.clone(),
            kind: kind.clone(),
            namespace: namespace.clone(),
            name: name.clone(),
            raw_lines: Vec::new(),
            lines: Vec::new(),
            mode,
            fullscreen: false,
            scroll: 0,
            loading: true,
            error: None,
        };
        k8s::spawn_yaml(self.state.clone(), context, kind, namespace, name, self.tx.clone());
    }

    fn open_logs_viewer(&mut self, context: String, namespace: String, pod: String) {
        if let Some(h) = self.log_handle.take() {
            h.abort();
        }
        self.log_gen += 1;
        let generation = self.log_gen;
        self.modal = Modal::LogsViewer {
            context: context.clone(),
            namespace: namespace.clone(),
            pod: pod.clone(),
            container: None,
            containers: Vec::new(),
            lines: Vec::new(),
            fullscreen: false,
            scroll: 0,
            follow: true,
            timestamps: false,
            generation,
            error: None,
        };
        k8s::spawn_containers(
            self.state.clone(),
            context.clone(),
            namespace.clone(),
            pod.clone(),
            self.tx.clone(),
        );
        self.log_handle = Some(k8s::spawn_logs(
            self.state.clone(),
            generation,
            context,
            namespace,
            pod,
            None,
            true,
            false,
            self.tx.clone(),
        ));
    }

    fn restart_logs_stream(&mut self) {
        if let Modal::LogsViewer {
            context,
            namespace,
            pod,
            container,
            lines,
            scroll,
            follow,
            timestamps,
            generation,
            error,
            ..
        } = &mut self.modal
        {
            if let Some(h) = self.log_handle.take() {
                h.abort();
            }
            self.log_gen += 1;
            *generation = self.log_gen;
            lines.clear();
            *scroll = 0;
            *error = None;
            self.log_handle = Some(k8s::spawn_logs(
                self.state.clone(),
                *generation,
                context.clone(),
                namespace.clone(),
                pod.clone(),
                container.clone(),
                *follow,
                *timestamps,
                self.tx.clone(),
            ));
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) {
        match &mut self.modal {
            Modal::None => {}
            Modal::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q')) {
                    self.modal = Modal::None;
                }
            }
            Modal::CommandPalette { query, selected } => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    let q = query.clone();
                    let cur = *selected;
                    let len = self.palette_items(&q).len();
                    if cur + 1 < len {
                        if let Modal::CommandPalette { selected, .. } = &mut self.modal {
                            *selected = cur + 1;
                        }
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    *selected = 0;
                }
                KeyCode::Char(ch) => {
                    query.push(ch);
                    *selected = 0;
                }
                KeyCode::Enter => {
                    let q = query.clone();
                    let sel = *selected;
                    let items = self.palette_items(&q);
                    if let Some(item) = items.get(sel).cloned() {
                        self.modal = Modal::None;
                        match item {
                            PaletteItem::Overview => {
                                self.screen = Screen::Overview;
                            }
                            PaletteItem::Utilization => {
                                self.open_utilization();
                            }
                            PaletteItem::Kind(k) => {
                                self.active_kind = k;
                                if self.selected_contexts.is_empty() {
                                    if let Some(c) = self.contexts.first().map(|x| x.name.clone()) {
                                        self.selected_contexts = vec![c];
                                    }
                                }
                                self.screen = Screen::Resources;
                                self.refresh_metadata_and_watch();
                            }
                            PaletteItem::Context(ctx) => {
                                self.open_single_cluster(ctx);
                            }
                            PaletteItem::SetClusterEnv => {
                                if let Some(ctx) = self.focused_context() {
                                    self.open_env_picker(ctx);
                                }
                            }
                            PaletteItem::ScanIssues => {
                                self.screen = Screen::Overview;
                                if let Some(ctx) = self.contexts.get(self.overview_cluster_cursor).map(|c| c.name.clone()) {
                                    self.scan_running = true;
                                    self.set_toast(format!("Scanning all resources in {ctx}..."), false);
                                    k8s::spawn_scan(self.state.clone(), ctx, self.tx.clone());
                                }
                            }
                            PaletteItem::ReloadKubeconfig => {
                                let report = self.state.reload();
                                self.contexts = report.contexts;
                                self.kube_errors = report.errors;
                                self.probe_all_overview();
                                self.set_toast("Reloaded kubeconfig", false);
                            }
                        }
                    } else {
                        self.modal = Modal::None;
                    }
                }
                _ => {}
            },
            Modal::NamespacePicker { query, selected } => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    let q = query.clone();
                    let cur = *selected;
                    let len = self.filtered_namespaces(&q).len();
                    if cur + 1 < len {
                        if let Modal::NamespacePicker { selected, .. } = &mut self.modal {
                            *selected = cur + 1;
                        }
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    *selected = 0;
                }
                KeyCode::Char(ch) => {
                    query.push(ch);
                    *selected = 0;
                }
                KeyCode::Enter => {
                    let q = query.clone();
                    let sel = *selected;
                    let items = self.filtered_namespaces(&q);
                    if let Some(ns) = items.get(sel).cloned() {
                        self.active_namespace = ns;
                        self.modal = Modal::None;
                        self.restart_watch();
                    }
                }
                _ => {}
            },
            Modal::YamlViewer {
                context,
                kind,
                namespace,
                name,
                raw_lines,
                lines,
                mode,
                fullscreen,
                scroll,
                ..
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.modal = Modal::None,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    if *scroll + 1 < lines.len() {
                        *scroll += 1;
                    }
                }
                KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
                KeyCode::PageDown => {
                    if !lines.is_empty() {
                        *scroll = (*scroll + 20).min(lines.len() - 1);
                    }
                }
                KeyCode::Char('g') => *scroll = 0,
                KeyCode::Char('G') => {
                    if !lines.is_empty() {
                        *scroll = lines.len() - 1;
                    }
                }
                KeyCode::Char('x') => {
                    *mode = if *mode == YamlViewMode::Decoded {
                        YamlViewMode::Raw
                    } else {
                        YamlViewMode::Decoded
                    };
                    *lines = yaml_lines_for_mode(raw_lines, *mode);
                    *scroll = if lines.is_empty() { 0 } else { (*scroll).min(lines.len() - 1) };
                }
                KeyCode::Char('i') => {
                    *mode = if *mode == YamlViewMode::Interpreted {
                        YamlViewMode::Raw
                    } else {
                        YamlViewMode::Interpreted
                    };
                    *lines = yaml_lines_for_mode(raw_lines, *mode);
                    *scroll = if lines.is_empty() { 0 } else { (*scroll).min(lines.len() - 1) };
                }
                KeyCode::Char('c') => {
                    let count = lines.len();
                    let content = lines.join("\n");
                    copy_to_clipboard(&content);
                    self.toast = Some((
                        format!("Copied {count} lines to clipboard"),
                        false,
                        std::time::Instant::now(),
                    ));
                }
                KeyCode::Char('z') => {
                    *fullscreen = !*fullscreen;
                }
                KeyCode::Char('E') => {
                    let yaml = raw_lines.join("\n");
                    self.pending_external = Some(PendingExternal::EditYaml {
                        context: context.clone(),
                        kind: kind.clone(),
                        namespace: namespace.clone(),
                        name: name.clone(),
                        yaml,
                    });
                }
                _ => {}
            },
            Modal::LogsViewer {
                lines,
                fullscreen,
                scroll,
                follow,
                timestamps,
                container,
                containers,
                ..
            } => {
                let mut need_restart = false;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        if let Some(h) = self.log_handle.take() {
                            h.abort();
                        }
                        self.modal = Modal::None;
                        return;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *follow = false;
                        *scroll = scroll.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *scroll + 1 < lines.len() {
                            *scroll += 1;
                        }
                    }
                    KeyCode::PageUp => {
                        *follow = false;
                        *scroll = scroll.saturating_sub(20);
                    }
                    KeyCode::PageDown => {
                        if !lines.is_empty() {
                            *scroll = (*scroll + 20).min(lines.len() - 1);
                        }
                    }
                    KeyCode::Char('g') => {
                        *follow = false;
                        *scroll = 0;
                    }
                    KeyCode::Char('G') => {
                        *follow = true;
                        *scroll = lines.len().saturating_sub(1);
                    }
                    KeyCode::Char('f') => {
                        *follow = !*follow;
                        if *follow {
                            *scroll = lines.len().saturating_sub(1);
                        }
                    }
                    KeyCode::Char('z') => {
                        *fullscreen = !*fullscreen;
                    }
                    KeyCode::Char('t') => {
                        *timestamps = !*timestamps;
                        need_restart = true;
                    }
                    KeyCode::Char('c') => {
                        if containers.len() > 1 {
                            let idx = container
                                .as_ref()
                                .and_then(|c| containers.iter().position(|x| x == c))
                                .map(|i| (i + 1) % containers.len())
                                .unwrap_or(0);
                            *container = Some(containers[idx].clone());
                            need_restart = true;
                        }
                    }
                    _ => {}
                }
                if need_restart {
                    self.restart_logs_stream();
                }
            }
            Modal::Confirm { kind, typed } => {
                let requires_name = matches!(kind, ConfirmKind::Delete { is_prod: true, .. });
                match key.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Backspace if requires_name => {
                        typed.pop();
                    }
                    KeyCode::Char(ch) if requires_name => {
                        typed.push(ch);
                    }
                    KeyCode::Char('y') | KeyCode::Char('Y') if !requires_name => {
                        let k = kind.clone();
                        self.modal = Modal::None;
                        self.execute_confirm(k);
                    }
                    KeyCode::Enter => {
                        if let ConfirmKind::Delete { is_prod: true, name, .. } = kind {
                            if typed != name {
                                return;
                            }
                        }
                        let k = kind.clone();
                        self.modal = Modal::None;
                        self.execute_confirm(k);
                    }
                    _ => {}
                }
            }
            Modal::ScalePrompt {
                context,
                kind,
                namespace,
                name,
                input,
            } => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(ch) if ch.is_ascii_digit() => {
                    input.push(ch);
                }
                KeyCode::Enter => {
                    if let Ok(replicas) = input.parse::<i32>() {
                        let (ctx, k, ns, nm) =
                            (context.clone(), kind.clone(), namespace.clone(), name.clone());
                        self.modal = Modal::None;
                        k8s::spawn_scale(self.state.clone(), ctx, k, ns, nm, replicas, self.tx.clone());
                    }
                }
                _ => {}
            },
            Modal::EnvPicker { context, selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.modal = Modal::None,
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    if *selected + 1 < EnvChoice::ALL.len() {
                        *selected += 1;
                    }
                }
                KeyCode::Enter => {
                    let ctx = context.clone();
                    let choice = EnvChoice::ALL.get(*selected).copied().unwrap_or(EnvChoice::Auto);
                    self.modal = Modal::None;
                    self.set_context_env(&ctx, choice);
                }
                _ => {}
            },
            Modal::DebugPrompt {
                context,
                namespace,
                name,
                is_node,
                selected_preset,
                custom_image,
                target,
                editing_target,
            } => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Tab if !*is_node => *editing_target = !*editing_target,
                KeyCode::Up if !*editing_target => {
                    *selected_preset = selected_preset.saturating_sub(1);
                }
                KeyCode::Down if !*editing_target => {
                    if *selected_preset < DEBUG_IMAGES.len() {
                        *selected_preset += 1;
                    }
                }
                KeyCode::Backspace => {
                    if *editing_target {
                        target.pop();
                    } else if *selected_preset == DEBUG_IMAGES.len() {
                        custom_image.pop();
                    }
                }
                KeyCode::Char(ch) => {
                    if *editing_target {
                        target.push(ch);
                    } else if *selected_preset == DEBUG_IMAGES.len() {
                        custom_image.push(ch);
                    }
                }
                KeyCode::Enter => {
                    let image = DEBUG_IMAGES
                        .get(*selected_preset)
                        .map(|s| (*s).to_string())
                        .unwrap_or_else(|| custom_image.trim().to_string());
                    if image.is_empty() {
                        return;
                    }
                    let tgt = if !*is_node && !target.trim().is_empty() {
                        Some(target.trim().to_string())
                    } else {
                        None
                    };
                    self.pending_external = Some(PendingExternal::Debug {
                        context: context.clone(),
                        namespace: namespace.clone(),
                        name: name.clone(),
                        is_node: *is_node,
                        image,
                        target: tgt,
                    });
                    self.modal = Modal::None;
                }
                _ => {}
            },
            Modal::PortForwardPrompt {
                context,
                namespace,
                kind,
                name,
                input,
                ports,
                selected_port,
                active,
                selected_active,
                focus_active,
                ..
            } => match key.code {
                KeyCode::Esc => self.modal = Modal::None,
                KeyCode::Tab => {
                    if !active.is_empty() {
                        *focus_active = !*focus_active;
                    }
                }
                KeyCode::Up => {
                    if *focus_active {
                        *selected_active = selected_active.saturating_sub(1);
                    } else {
                        *selected_port = selected_port.saturating_sub(1);
                    }
                }
                KeyCode::Down => {
                    if *focus_active {
                        if *selected_active + 1 < active.len() {
                            *selected_active += 1;
                        }
                    } else if *selected_port + 1 < ports.len() {
                        *selected_port += 1;
                    }
                }
                KeyCode::Delete => {
                    if let Some(f) = active.get(*selected_active).cloned() {
                        k8s::stop_port_forward(&self.state, f.id);
                        *active = k8s::list_port_forwards(&self.state);
                        if !active.is_empty() {
                            *selected_active = (*selected_active).min(active.len() - 1);
                        } else {
                            *selected_active = 0;
                            *focus_active = false;
                        }
                        self.set_toast(
                            format!("Stopped forward localhost:{}", f.local_port),
                            false,
                        );
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(ch) if ch.is_ascii_digit() || matches!(ch, ':' | '.' | '[' | ']') => {
                    input.push(ch);
                }
                KeyCode::Enter => {
                    let default_remote = ports.get(*selected_port).map(|p| p.port);
                    match parse_port_forward_input(input, default_remote) {
                        Ok(target) => {
                            let (ctx, ns, k, nm) = (
                                context.clone(),
                                namespace.clone(),
                                kind.clone(),
                                name.clone(),
                            );
                            self.modal = Modal::None;
                            k8s::spawn_port_forward(
                                self.state.clone(),
                                ctx,
                                ns,
                                k,
                                nm,
                                target.remote_port,
                                target.local_port,
                                target.bind_addr,
                                self.tx.clone(),
                            );
                        }
                        Err(e) => self.set_toast(e, true),
                    }
                }
                _ => {}
            },
        }
    }

    fn execute_confirm(&mut self, kind: ConfirmKind) {
        match kind {
            ConfirmKind::Delete {
                context,
                kind,
                namespace,
                name,
                ..
            } => k8s::spawn_delete(self.state.clone(), context, kind, namespace, name, self.tx.clone()),
            ConfirmKind::Restart {
                context,
                kind,
                namespace,
                name,
            } => k8s::spawn_restart(self.state.clone(), context, kind, namespace, name, self.tx.clone()),
            ConfirmKind::Cordon {
                context,
                name,
                cordon,
            } => k8s::spawn_cordon(self.state.clone(), context, name, cordon, self.tx.clone()),
            ConfirmKind::SuspendCronJob {
                context,
                namespace,
                name,
                suspend,
            } => k8s::spawn_suspend_cronjob(
                self.state.clone(),
                context,
                namespace,
                name,
                suspend,
                self.tx.clone(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_app() -> App {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("steerglass-tui-unit-test-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = Arc::new(AppState::new(dir.join("settings.json")));
        let (tx, _rx) = mpsc::unbounded_channel();
        App::new(state, tx, None)
    }

    fn sample_row(uid: &str, name: &str, ns: &str, health: Health, cells: Vec<&str>) -> Row {
        Row {
            uid: uid.into(),
            name: name.into(),
            namespace: Some(ns.into()),
            created: None,
            cells: cells.into_iter().map(String::from).collect(),
            health,
            last_restart: None,
        }
    }

    #[tokio::test]
    async fn filters_rows_by_status_and_text() {
        let mut app = test_app();
        app.selected_contexts = vec!["prod-us".into(), "staging-eu".into()];

        let mut prod_map = HashMap::new();
        prod_map.insert(
            "1".into(),
            sample_row("1", "api-1", "default", Health::Ok, vec!["1/1", "Running"]),
        );
        prod_map.insert(
            "2".into(),
            sample_row("2", "worker-1", "jobs", Health::Error, vec!["0/1", "CrashLoopBackOff"]),
        );

        let mut stg_map = HashMap::new();
        stg_map.insert(
            "3".into(),
            sample_row("3", "web-1", "default", Health::Warn, vec!["0/1", "Pending"]),
        );

        app.rows_by_ctx.insert("prod-us".into(), prod_map);
        app.rows_by_ctx.insert("staging-eu".into(), stg_map);

        assert_eq!(app.filtered_rows().len(), 3);

        app.filter_text = "status:bad".into();
        let bad = app.filtered_rows();
        assert_eq!(bad.len(), 2);
        assert!(bad.iter().all(|r| r.row.health != Health::Ok));

        app.filter_text = "status:error".into();
        let errs = app.filtered_rows();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].row.name, "worker-1");

        app.filter_text = "staging-eu".into();
        let stg = app.filtered_rows();
        assert_eq!(stg.len(), 1);
        assert_eq!(stg[0].row.name, "web-1");
    }

    #[tokio::test]
    async fn prod_delete_requires_exact_name() {
        let mut app = test_app();
        app.modal = Modal::Confirm {
            kind: ConfirmKind::Delete {
                context: "prod-us".into(),
                kind: k8s::builtin_kinds().into_iter().next().unwrap(),
                namespace: Some("default".into()),
                name: "my-pod".into(),
                is_prod: true,
            },
            typed: "my-po".into(),
        };

        // Pressing Enter before typing full name keeps modal open.
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.modal, Modal::Confirm { .. }));

        // Typing the final character and pressing Enter submits.
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.modal, Modal::None));
    }

    #[tokio::test]
    async fn initial_context_opens_single_cluster_with_multiple_contexts() {
        let state = Arc::new(AppState::new(
            std::env::temp_dir().join("steerglass-tui-unit-test/settings.json"),
        ));
        {
            let mut report = state.report.write();
            report.contexts = vec![
                crate::core::kubeconfig::ContextInfo {
                    name: "ctx-a".into(),
                    cluster: "cluster-a".into(),
                    user: Some("user-a".into()),
                    namespace: None,
                    server: None,
                    source: "test".into(),
                    is_current: true,
                },
                crate::core::kubeconfig::ContextInfo {
                    name: "ctx-b".into(),
                    cluster: "cluster-b".into(),
                    user: Some("user-b".into()),
                    namespace: None,
                    server: None,
                    source: "test".into(),
                    is_current: false,
                },
            ];
        }
        let (tx, _rx) = mpsc::unbounded_channel();
        let app = App::new(state.clone(), tx.clone(), Some("ctx-b".into()));
        assert_eq!(app.screen, Screen::Resources);
        assert_eq!(app.selected_contexts, vec!["ctx-b".to_string()]);
        assert_eq!(app.sidebar_cursor, 2);

        let app_missing = App::new(state, tx, Some("nonexistent".into()));
        assert_eq!(app_missing.screen, Screen::Overview);
        assert!(matches!(app_missing.toast, Some((_, true, _))));
    }

    #[tokio::test]
    async fn reorder_contexts_persists_and_updates_numeric_shortcuts() {
        let dir = std::env::temp_dir().join("steerglass-tui-reorder-test");
        let _ = std::fs::remove_dir_all(&dir);
        let settings_path = dir.join("settings.json");
        let state = Arc::new(AppState::new(settings_path.clone()));
        {
            let mut report = state.report.write();
            report.contexts = vec![
                crate::core::kubeconfig::ContextInfo {
                    name: "abb".into(),
                    cluster: "cluster-a".into(),
                    user: None,
                    namespace: None,
                    server: None,
                    source: "test".into(),
                    is_current: true,
                },
                crate::core::kubeconfig::ContextInfo {
                    name: "internal".into(),
                    cluster: "cluster-b".into(),
                    user: None,
                    namespace: None,
                    server: None,
                    source: "test".into(),
                    is_current: false,
                },
            ];
        }
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(state.clone(), tx.clone(), None);
        app.focus = Focus::Sidebar;
        app.sidebar_cursor = 2; // "internal"

        // Move "internal" up to slot 1
        app.handle_key(KeyEvent::from(KeyCode::Char('K')));
        assert_eq!(app.contexts[0].name, "internal");
        assert_eq!(app.contexts[1].name, "abb");
        assert_eq!(app.sidebar_cursor, 1);

        // Pressing '1' now opens "internal"
        app.handle_key(KeyEvent::from(KeyCode::Char('1')));
        assert_eq!(app.selected_contexts, vec!["internal".to_string()]);

        // Persisted order is restored in a fresh AppState
        let state2 = Arc::new(AppState::new(settings_path));
        {
            let mut report = state2.report.write();
            report.contexts = app.contexts.clone();
            report.contexts.reverse(); // simulate original kubeconfig order
        }
        let app2 = App::new(state2, tx, None);
        assert_eq!(app2.contexts[0].name, "internal");
        assert_eq!(app2.contexts[1].name, "abb");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ctrl_z_requests_suspend() {
        let mut app = test_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert!(matches!(app.pending_external, Some(PendingExternal::Suspend)));
    }

    #[tokio::test]
    async fn shift_a_cycles_age_sort_newest_oldest_name() {
        let mut app = test_app();
        app.screen = Screen::Resources;
        app.focus = Focus::Main;
        app.selected_contexts = vec!["ctx".into()];

        let mut map = HashMap::new();
        let mut older = sample_row("1", "alpha", "default", Health::Ok, vec![]);
        older.created = Some("2026-11-26T10:00:00Z".into());
        let mut newer = sample_row("2", "zeta", "default", Health::Ok, vec![]);
        newer.created = Some("2026-11-26T12:00:00Z".into());
        let no_ts = sample_row("3", "beta", "default", Health::Ok, vec![]);
        map.insert("1".into(), older);
        map.insert("2".into(), newer);
        map.insert("3".into(), no_ts);
        app.rows_by_ctx.insert("ctx".into(), map);

        // Default: alphabetical by name
        let names: Vec<_> = app.filtered_rows().into_iter().map(|r| r.row.name).collect();
        assert_eq!(names, vec!["alpha", "beta", "zeta"]);

        // First Shift+A: newest first, missing timestamp last
        app.handle_key(KeyEvent::from(KeyCode::Char('A')));
        let names: Vec<_> = app.filtered_rows().into_iter().map(|r| r.row.name).collect();
        assert_eq!(names, vec!["zeta", "alpha", "beta"]);

        // Second Shift+A: oldest first, missing timestamp last
        app.handle_key(KeyEvent::from(KeyCode::Char('A')));
        let names: Vec<_> = app.filtered_rows().into_iter().map(|r| r.row.name).collect();
        assert_eq!(names, vec!["alpha", "zeta", "beta"]);

        // Third Shift+A: back to name order
        app.handle_key(KeyEvent::from(KeyCode::Char('A')));
        let names: Vec<_> = app.filtered_rows().into_iter().map(|r| r.row.name).collect();
        assert_eq!(names, vec!["alpha", "beta", "zeta"]);
    }

    #[tokio::test]
    async fn env_picker_changes_classification_persists_and_updates_prod_guard() {
        let dir = std::env::temp_dir().join("steerglass-tui-env-test");
        let _ = std::fs::remove_dir_all(&dir);
        let settings_path = dir.join("settings.json");

        let state = Arc::new(AppState::new(settings_path.clone()));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(state, tx, None);
        app.contexts = vec![
            ContextInfo {
                name: "my-cluster".into(),
                cluster: "c1".into(),
                user: None,
                namespace: None,
                server: None,
                source: "kubeconfig".into(),
                is_current: true,
            },
            ContextInfo {
                name: "api-prod".into(),
                cluster: "c2".into(),
                user: None,
                namespace: None,
                server: None,
                source: "kubeconfig".into(),
                is_current: false,
            },
        ];
        app.screen = Screen::Overview;
        app.focus = Focus::Main;
        app.overview_pane = OverviewPane::Clusters;
        app.overview_cluster_cursor = 0;

        assert_eq!(app.context_env("my-cluster"), None);
        assert_eq!(app.context_env("api-prod"), Some(Env::Prod));

        // Press 'e' on "my-cluster" -> opens EnvPicker, select PROD (index 1), press Enter.
        app.handle_key(KeyEvent::from(KeyCode::Char('e')));
        assert!(matches!(app.modal, Modal::EnvPicker { ref context, selected: 0 } if context == "my-cluster"));
        app.handle_key(KeyEvent::from(KeyCode::Down));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.modal, Modal::None));
        assert_eq!(app.context_env("my-cluster"), Some(Env::Prod));

        // Override "api-prod" to None (index 5) via sidebar 'e'.
        app.focus = Focus::Sidebar;
        app.sidebar_cursor = 2; // 0 is Overview, 1 is my-cluster, 2 is api-prod
        app.handle_key(KeyEvent::from(KeyCode::Char('e')));
        for _ in 0..5 {
            app.handle_key(KeyEvent::from(KeyCode::Down));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.context_env("api-prod"), None);

        // Delete confirmation on "my-cluster" now requires typing the resource name (is_prod == true).
        app.screen = Screen::Resources;
        app.focus = Focus::Main;
        app.selected_contexts = vec!["my-cluster".into()];
        let mut map = HashMap::new();
        map.insert("1".into(), sample_row("1", "web", "default", Health::Ok, vec![]));
        app.rows_by_ctx.insert("my-cluster".into(), map);
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert!(matches!(
            app.modal,
            Modal::Confirm {
                kind: ConfirmKind::Delete { is_prod: true, .. },
                ..
            }
        ));
        app.modal = Modal::None;

        // Persisted overrides survive a fresh AppState load.
        let state2 = Arc::new(AppState::new(settings_path));
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        let app2 = App::new(state2, tx2, None);
        assert_eq!(app2.context_env("my-cluster"), Some(Env::Prod));
        assert_eq!(app2.context_env("api-prod"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn debug_prompt_launches_pod_and_node_debug_sessions() {
        let mut app = test_app();
        app.screen = Screen::Resources;
        app.focus = Focus::Main;
        app.selected_contexts = vec!["dev-ctx".into()];

        let mut map = HashMap::new();
        map.insert("1".into(), sample_row("1", "api-0", "app", Health::Ok, vec![]));
        app.rows_by_ctx.insert("dev-ctx".into(), map);

        // Pod: Shift+D opens DebugPrompt, select netshoot (index 1), Tab to target, type "api", Enter.
        app.handle_key(KeyEvent::from(KeyCode::Char('D')));
        assert!(matches!(
            app.modal,
            Modal::DebugPrompt {
                ref name,
                is_node: false,
                selected_preset: 0,
                ..
            } if name == "api-0"
        ));
        app.handle_key(KeyEvent::from(KeyCode::Down));
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        for ch in "api".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(ch)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.modal, Modal::None));
        assert!(matches!(
            app.pending_external,
            Some(PendingExternal::Debug {
                ref context,
                namespace: Some(ref ns),
                ref name,
                is_node: false,
                ref image,
                target: Some(ref tgt),
            }) if context == "dev-ctx" && ns == "app" && name == "api-0" && image == "nicolaka/netshoot:latest" && tgt == "api"
        ));

        // Node: Shift+D opens DebugPrompt with is_node = true, choose custom image.
        app.pending_external = None;
        app.active_kind = KindRef {
            group: String::new(),
            version: "v1".into(),
            kind: "Node".into(),
            plural: "nodes".into(),
            namespaced: false,
            verbs: vec![],
        };
        let mut node_map = HashMap::new();
        let mut node_row = sample_row("n1", "worker-1", "", Health::Ok, vec![]);
        node_row.namespace = None;
        node_map.insert("n1".into(), node_row);
        app.rows_by_ctx.insert("dev-ctx".into(), node_map);

        app.handle_key(KeyEvent::from(KeyCode::Char('D')));
        for _ in 0..DEBUG_IMAGES.len() {
            app.handle_key(KeyEvent::from(KeyCode::Down));
        }
        for ch in "debian:bookworm".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(ch)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(
            app.pending_external,
            Some(PendingExternal::Debug {
                ref name,
                is_node: true,
                ref image,
                target: None,
                ..
            }) if name == "worker-1" && image == "debian:bookworm"
        ));
    }

    #[tokio::test]
    async fn view_state_resumes_when_cli_args_match_and_resets_when_different() {
        let dir = std::env::temp_dir().join("steerglass-tui-resume-test");
        let _ = std::fs::remove_dir_all(&dir);
        let settings_path = dir.join("settings.json");

        let contexts = vec![
            ContextInfo {
                name: "ctx-a".into(),
                cluster: "c1".into(),
                user: None,
                namespace: None,
                server: None,
                source: "kubeconfig".into(),
                is_current: true,
            },
            ContextInfo {
                name: "ctx-b".into(),
                cluster: "c2".into(),
                user: None,
                namespace: None,
                server: None,
                source: "kubeconfig".into(),
                is_current: false,
            },
        ];

        // First run: no CLI args, customize view settings and filters, then quit.
        {
            let state = Arc::new(AppState::new(settings_path.clone()));
            state.report.write().contexts = contexts.clone();
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new_with_cli(state, tx, None, None);
            app.open_single_cluster("ctx-b".into());
            app.active_namespace = Some("payments".into());
            app.filter_text = "status:bad".into();
            app.resource_sort = ResourceSort::AgeNewestFirst;
            app.show_sidebar = false;
            app.active_kind = KindRef {
                group: "apps".into(),
                version: "v1".into(),
                kind: "Deployment".into(),
                plural: "deployments".into(),
                namespaced: true,
                verbs: vec![],
            };
            app.handle_key(KeyEvent::from(KeyCode::Char('q')));
            assert!(app.should_quit);
        }

        // Second run with matching CLI args (None, None): resumes saved state.
        {
            let state = Arc::new(AppState::new(settings_path.clone()));
            state.report.write().contexts = contexts.clone();
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let app = App::new_with_cli(state, tx, None, None);
            assert_eq!(app.screen, Screen::Resources);
            assert_eq!(app.selected_contexts, vec!["ctx-b".to_string()]);
            assert_eq!(app.active_namespace.as_deref(), Some("payments"));
            assert_eq!(app.filter_text, "status:bad");
            assert_eq!(app.resource_sort, ResourceSort::AgeNewestFirst);
            assert!(!app.show_sidebar);
            assert_eq!(app.active_kind.kind, "Deployment");
        }

        // Third run with different CLI args (-c ctx-a): does NOT restore old view/filters.
        {
            let state = Arc::new(AppState::new(settings_path.clone()));
            state.report.write().contexts = contexts.clone();
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let app = App::new_with_cli(state, tx, Some("ctx-a".into()), None);
            assert_eq!(app.screen, Screen::Resources);
            assert_eq!(app.selected_contexts, vec!["ctx-a".to_string()]);
            assert_eq!(app.active_namespace, None);
            assert_eq!(app.filter_text, "");
            assert_eq!(app.resource_sort, ResourceSort::NameAsc);
            assert!(app.show_sidebar);
            assert_eq!(app.active_kind.kind, "Pod");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mouse_drag_resizes_panes_and_persists() {
        let dir = std::env::temp_dir().join("steerglass-tui-mouse-resize-test");
        let _ = std::fs::remove_dir_all(&dir);
        let settings_path = dir.join("settings.json");

        let state = Arc::new(AppState::new(settings_path.clone()));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new_with_cli(state, tx, None, None);
        app.screen = Screen::Overview;
        app.show_sidebar = true;
        app.term_size = (120, 45); // body_top = 3, body_bottom = 43, body_h = 40

        // Drag sidebar border from x=30 to x=41 -> sidebar_width = 42
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 30,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.dragging, Some(DragTarget::Sidebar));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 41,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 41,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.dragging, None);
        assert_eq!(app.sidebar_width, 42);

        // Drag overview horizontal split from y = 3 + (40*48/100) = 22 to y = 31 (rel_y=28 -> 70%)
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 22,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.dragging, Some(DragTarget::OverviewSplit));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 60,
            row: 31,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 60,
            row: 31,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.dragging, None);
        assert_eq!(app.overview_split_pct, 70);

        // Persisted pane sizes are restored on restart
        let state2 = Arc::new(AppState::new(settings_path));
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        let app2 = App::new_with_cli(state2, tx2, None, None);
        assert_eq!(app2.sidebar_width, 42);
        assert_eq!(app2.overview_split_pct, 70);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mouse_click_focuses_pane() {
        let mut app = test_app();
        app.screen = Screen::Overview;
        app.show_sidebar = true;
        app.focus = Focus::Main;
        app.overview_pane = OverviewPane::Clusters;
        app.term_size = (120, 45); // sidebar_width=30, body_top=3, split_y=22

        // Click inside sidebar
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.focus, Focus::Sidebar);

        // Click inside bottom overview pane (Issues)
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 35,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.focus, Focus::Main);
        assert_eq!(app.overview_pane, OverviewPane::Issues);

        // Click inside top overview pane (Clusters)
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.focus, Focus::Main);
        assert_eq!(app.overview_pane, OverviewPane::Clusters);
    }

    #[tokio::test]
    async fn palette_matches_type_prefix_without_colon() {
        let mut app = test_app();
        app.contexts = vec![
            ContextInfo {
                name: "Internal".into(),
                cluster: "c1".into(),
                user: None,
                namespace: None,
                server: None,
                source: "kubeconfig".into(),
                is_current: true,
            },
            ContextInfo {
                name: "prod-internal".into(),
                cluster: "c2".into(),
                user: None,
                namespace: None,
                server: None,
                source: "kubeconfig".into(),
                is_current: false,
            },
        ];

        let labels: Vec<String> = app
            .palette_items("cluster int")
            .into_iter()
            .map(|i| i.label())
            .collect();
        assert_eq!(labels, vec!["Cluster: Internal", "Cluster: prod-internal"]);

        let res_labels: Vec<String> = app
            .palette_items("res dep")
            .into_iter()
            .map(|i| i.label())
            .collect();
        assert!(res_labels.iter().any(|l| l == "Resource: Deployment (apps)"));
        assert!(!res_labels.iter().any(|l| l.starts_with("Cluster:")));

        let single_labels: Vec<String> = app
            .palette_items("Internal")
            .into_iter()
            .map(|i| i.label())
            .collect();
        assert_eq!(
            single_labels,
            vec!["Cluster: Internal", "Cluster: prod-internal"]
        );
    }

    #[tokio::test]
    async fn x_decodes_secret_data_in_yaml_viewer() {
        let yaml = [
            "apiVersion: v1",
            "kind: Secret",
            "metadata:",
            "  name: db-creds",
            "data:",
            "  username: YWRtaW4=",
            "  cert: 'bGluZTEKbGluZTI='",
            "  bin: /+8=",
        ]
        .join("\n");

        let mut app = test_app();
        app.screen = Screen::Resources;
        app.focus = Focus::Main;
        app.selected_contexts = vec!["ctx".into()];
        app.active_kind = KindRef {
            group: String::new(),
            version: "v1".into(),
            kind: "Secret".into(),
            plural: "secrets".into(),
            namespaced: true,
            verbs: vec![],
        };
        let mut map = HashMap::new();
        map.insert("1".into(), sample_row("1", "db-creds", "default", Health::Ok, vec![]));
        app.rows_by_ctx.insert("ctx".into(), map);

        // Press 'x' on Secret row -> opens YamlViewer in decoded mode
        app.handle_key(KeyEvent::from(KeyCode::Char('x')));
        app.handle_event(AppEvent::YamlLoaded {
            context: "ctx".into(),
            name: "db-creds".into(),
            namespace: Some("default".into()),
            kind: app.active_kind.clone(),
            result: Ok(yaml.clone()),
        });

        match &app.modal {
            Modal::YamlViewer { lines, mode, .. } => {
                assert_eq!(*mode, YamlViewMode::Decoded);
                assert!(lines.contains(&"  username: admin".to_string()));
                assert!(lines.contains(&"  cert: |".to_string()));
                assert!(lines.contains(&"    line1".to_string()));
                assert!(lines.contains(&"    line2".to_string()));
                assert!(lines.contains(&"  bin: <binary: 2 bytes>".to_string()));
                // metadata is untouched
                assert!(lines.contains(&"  name: db-creds".to_string()));
            }
            _ => panic!("expected YamlViewer"),
        }

        // Press 'x' again inside YamlViewer -> toggles back to raw base64
        app.handle_key(KeyEvent::from(KeyCode::Char('x')));
        match &app.modal {
            Modal::YamlViewer { lines, mode, .. } => {
                assert_eq!(*mode, YamlViewMode::Raw);
                assert!(lines.contains(&"  username: YWRtaW4=".to_string()));
            }
            _ => panic!("expected YamlViewer"),
        }

        // Press 'i' inside YamlViewer -> switches to Interpreted mode
        app.handle_key(KeyEvent::from(KeyCode::Char('i')));
        match &app.modal {
            Modal::YamlViewer { mode, .. } => {
                assert_eq!(*mode, YamlViewMode::Interpreted);
            }
            _ => panic!("expected YamlViewer"),
        }

        // Press 'z' inside YamlViewer -> toggles fullscreen
        app.handle_key(KeyEvent::from(KeyCode::Char('z')));
        assert!(matches!(app.modal, Modal::YamlViewer { fullscreen: true, .. }));
        app.handle_key(KeyEvent::from(KeyCode::Char('z')));
        assert!(matches!(app.modal, Modal::YamlViewer { fullscreen: false, .. }));

        // Press 'c' inside YamlViewer -> copies displayed content and shows toast
        app.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|(msg, is_err, _)| !is_err && msg.contains("Copied"))
        );
    }

    #[tokio::test]
    async fn utilization_view_sorts_nodes_and_pods_by_cpu_and_memory() {
        let mut app = test_app();
        app.selected_contexts = vec!["ctx".into()];
        app.handle_key(KeyEvent::from(KeyCode::Char('u')));
        assert_eq!(app.screen, Screen::Utilization);

        app.handle_event(AppEvent::UtilizationLoaded {
            context: "ctx".into(),
            result: Ok(k8s::ClusterUtilization {
                metrics_available: true,
                nodes: vec![
                    k8s::NodeUsageEntry {
                        name: "node-a".into(),
                        cpu_used: Some(0.5),
                        cpu_req: 0.4,
                        cpu_alloc: 4.0,
                        mem_used: Some(4.0 * 1073741824.0),
                        mem_req: 2.0 * 1073741824.0,
                        mem_alloc: 8.0 * 1073741824.0,
                        pods: 3,
                    },
                    k8s::NodeUsageEntry {
                        name: "node-b".into(),
                        cpu_used: Some(2.0),
                        cpu_req: 1.5,
                        cpu_alloc: 4.0,
                        mem_used: Some(1.0 * 1073741824.0),
                        mem_req: 0.5 * 1073741824.0,
                        mem_alloc: 8.0 * 1073741824.0,
                        pods: 5,
                    },
                ],
                pods: vec![
                    k8s::PodUsageEntry {
                        namespace: "default".into(),
                        name: "mem-hog".into(),
                        node: Some("node-a".into()),
                        cpu_used: Some(0.1),
                        cpu_req: 0.1,
                        cpu_lim: None,
                        mem_used: Some(2.0 * 1073741824.0),
                        mem_req: 1.0 * 1073741824.0,
                        mem_lim: None,
                    },
                    k8s::PodUsageEntry {
                        namespace: "default".into(),
                        name: "cpu-hog".into(),
                        node: Some("node-b".into()),
                        cpu_used: Some(1.5),
                        cpu_req: 1.0,
                        cpu_lim: Some(2.0),
                        mem_used: Some(128.0 * 1048576.0),
                        mem_req: 128.0 * 1048576.0,
                        mem_lim: Some(256.0 * 1048576.0),
                    },
                ],
            }),
        });

        // Default sort: CPU desc -> node-b, cpu-hog first
        assert_eq!(app.utilization_nodes()[0].1.name, "node-b");
        assert_eq!(app.utilization_pods()[0].1.name, "cpu-hog");

        // Press 'm' -> Memory desc -> node-a, mem-hog first
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(app.utilization_nodes()[0].1.name, "node-a");
        assert_eq!(app.utilization_pods()[0].1.name, "mem-hog");

        // Filter with '/' -> filters both nodes and pods (e.g. by node name or pod name)
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        for ch in "node-b".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(ch)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.utilization_nodes().len(), 1);
        assert_eq!(app.utilization_nodes()[0].1.name, "node-b");
        assert_eq!(app.utilization_pods().len(), 1);
        assert_eq!(app.utilization_pods()[0].1.name, "cpu-hog");
    }

    #[tokio::test]
    async fn shift_f_opens_port_forward_prompt_and_parses_ports() {
        assert_eq!(
            parse_port_forward_input("8080", None),
            Ok(PortForwardTarget {
                bind_addr: "127.0.0.1".into(),
                local_port: None,
                remote_port: 8080,
            })
        );
        assert_eq!(
            parse_port_forward_input("8080:80", None),
            Ok(PortForwardTarget {
                bind_addr: "127.0.0.1".into(),
                local_port: Some(8080),
                remote_port: 80,
            })
        );
        assert_eq!(
            parse_port_forward_input("0.0.0.0:9090:80", None),
            Ok(PortForwardTarget {
                bind_addr: "0.0.0.0".into(),
                local_port: Some(9090),
                remote_port: 80,
            })
        );
        assert_eq!(
            parse_port_forward_input("[::1]:9090:80", None),
            Ok(PortForwardTarget {
                bind_addr: "::1".into(),
                local_port: Some(9090),
                remote_port: 80,
            })
        );
        assert_eq!(
            parse_port_forward_input("0.0.0.0", Some(8080)),
            Ok(PortForwardTarget {
                bind_addr: "0.0.0.0".into(),
                local_port: None,
                remote_port: 8080,
            })
        );
        assert!(parse_port_forward_input("", None).is_err());
        assert!(parse_port_forward_input("0", None).is_err());

        let mut app = test_app();
        app.screen = Screen::Resources;
        app.focus = Focus::Main;
        app.selected_contexts = vec!["ctx".into()];
        let mut map = HashMap::new();
        map.insert("1".into(), sample_row("1", "web-0", "default", Health::Ok, vec![]));
        app.rows_by_ctx.insert("ctx".into(), map);

        app.handle_key(KeyEvent::from(KeyCode::Char('F')));
        assert!(matches!(
            app.modal,
            Modal::PortForwardPrompt {
                ref kind,
                ref name,
                ..
            } if kind == "Pod" && name == "web-0"
        ));
    }
}
