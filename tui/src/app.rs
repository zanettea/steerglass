use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;

use crate::core::health::{Issue, Scan};
use crate::core::kubeconfig::ContextInfo;
use crate::core::rows::{Health, Row};
use crate::core::state::{AppState, KindRef};
use crate::env::{Env, detect_env};
use crate::k8s::{self, AppEvent, ClusterSummary, WatchUpdate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Resources,
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

#[derive(Debug, Clone)]
pub struct TableRow {
    pub context: String,
    pub row: Row,
}

#[derive(Debug, Clone)]
pub struct ClusterIssue {
    pub context: String,
    pub issue: Issue,
}

#[derive(Debug, Clone)]
pub enum PendingExternal {
    Shell {
        context: String,
        namespace: String,
        pod: String,
        container: Option<String>,
    },
    EditYaml {
        context: String,
        kind: KindRef,
        namespace: Option<String>,
        name: String,
        yaml: String,
    },
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
        lines: Vec<String>,
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
}

#[derive(Debug, Clone)]
pub enum PaletteItem {
    Overview,
    Kind(KindRef),
    Context(String),
    ScanIssues,
    ReloadKubeconfig,
}

impl PaletteItem {
    pub fn label(&self) -> String {
        match self {
            PaletteItem::Overview => "Go to Overview".into(),
            PaletteItem::Kind(k) => {
                if k.group.is_empty() {
                    format!("Resource: {}", k.kind)
                } else {
                    format!("Resource: {} ({})", k.kind, k.group)
                }
            }
            PaletteItem::Context(c) => format!("Cluster: {c}"),
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

    pub filter_active: bool,
    pub filter_text: String,

    pub log_gen: u64,
    pub log_handle: Option<AbortHandle>,

    pub modal: Modal,
    pub toast: Option<(String, bool, std::time::Instant)>,
    pub tick_count: usize,
}

impl App {
    pub fn new(
        state: Arc<AppState>,
        tx: UnboundedSender<AppEvent>,
        initial_context: Option<String>,
    ) -> Self {
        let report = state.report.read().clone();
        let contexts = report.contexts;
        let kube_errors = report.errors;

        let initial_ctx = initial_context
            .filter(|c| contexts.iter().any(|x| &x.name == c))
            .or_else(|| contexts.iter().find(|c| c.is_current).map(|c| c.name.clone()))
            .or_else(|| contexts.first().map(|c| c.name.clone()));

        let selected_contexts = initial_ctx.clone().into_iter().collect::<Vec<_>>();
        let screen = if selected_contexts.is_empty() {
            Screen::Overview
        } else if contexts.len() > 1 && initial_ctx.is_none() {
            Screen::Overview
        } else {
            Screen::Overview
        };

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
            filter_active: false,
            filter_text: String::new(),
            log_gen: 0,
            log_handle: None,
            modal: Modal::None,
            toast: None,
            tick_count: 0,
        };

        app.probe_all_overview();
        if let Some(ctx) = initial_ctx {
            if let Some(pos) = app.contexts.iter().position(|c| c.name == ctx) {
                app.sidebar_cursor = pos + 1;
            }
            if app.contexts.len() == 1 {
                app.open_single_cluster(ctx);
            }
        }
        app
    }

    pub fn set_toast(&mut self, msg: impl Into<String>, is_err: bool) {
        self.toast = Some((msg.into(), is_err, std::time::Instant::now()));
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
            a.row
                .name
                .cmp(&b.row.name)
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

    pub fn palette_items(&self, query: &str) -> Vec<PaletteItem> {
        let mut items = Vec::new();
        items.push(PaletteItem::Overview);
        for k in self.available_kinds() {
            items.push(PaletteItem::Kind(k));
        }
        for c in &self.contexts {
            items.push(PaletteItem::Context(c.name.clone()));
        }
        items.push(PaletteItem::ScanIssues);
        items.push(PaletteItem::ReloadKubeconfig);

        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            items.retain(|i| i.label().to_lowercase().contains(&q));
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
                    lines,
                    loading,
                    error,
                    ..
                } = &mut self.modal
                {
                    if *mc == context && *mn == name && *mns == namespace && mk.kind == kind.kind {
                        *loading = false;
                        match result {
                            Ok(yaml) => {
                                *lines = yaml.lines().map(ToString::to_string).collect();
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

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
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
                }
                KeyCode::Enter => {
                    self.filter_active = false;
                }
                KeyCode::Backspace => {
                    self.filter_text.pop();
                    self.table_cursor = 0;
                }
                KeyCode::Char(ch) => {
                    self.filter_text.push(ch);
                    self.table_cursor = 0;
                }
                _ => {}
            }
            return;
        }

        // Global shortcuts
        match key.code {
            KeyCode::Char('q') => {
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
            KeyCode::Char('R') => {
                let report = self.state.reload();
                self.contexts = report.contexts;
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
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) {
        let total = 1 + self.contexts.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.sidebar_cursor = self.sidebar_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.sidebar_cursor + 1 < total {
                    self.sidebar_cursor += 1;
                }
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
                    self.overview_cluster_cursor = self.overview_cluster_cursor.saturating_sub(1);
                }
                OverviewPane::Issues => {
                    self.overview_issue_cursor = self.overview_issue_cursor.saturating_sub(1);
                }
            },
            KeyCode::Down | KeyCode::Char('j') => match self.overview_pane {
                OverviewPane::Clusters => {
                    if self.overview_cluster_cursor + 1 < self.contexts.len() {
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
            KeyCode::Enter | KeyCode::Char('y') => {
                if let Some(tr) = rows.get(self.table_cursor) {
                    self.open_yaml_viewer(tr.context.clone(), tr.row.namespace.clone(), tr.row.name.clone());
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
                    let is_prod = detect_env(&tr.context) == Some(Env::Prod);
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

    fn open_yaml_viewer(&mut self, context: String, namespace: Option<String>, name: String) {
        let kind = self.active_kind.clone();
        self.modal = Modal::YamlViewer {
            context: context.clone(),
            kind: kind.clone(),
            namespace: namespace.clone(),
            name: name.clone(),
            lines: Vec::new(),
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
                lines,
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
                KeyCode::Char('E') => {
                    let yaml = lines.join("\n");
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
        let state = Arc::new(AppState::new(
            std::env::temp_dir().join("steerglass-tui-unit-test/settings.json"),
        ));
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
}
