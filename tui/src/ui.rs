use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row as TuiRow, Table,
    TableState, Wrap,
};

use crate::app::{
    App, ConfirmKind, ContainerPurpose, Focus, Modal, OverviewPane, Screen, UtilizationPane,
    UtilizationSort, YamlViewMode,
};
use crate::core::health::Severity;
use crate::core::rows::{self, Health};
use crate::env::{EnvChoice, detect_env};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

pub fn render(f: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(f.area());

    render_header(f, app, root[0]);

    if app.show_sidebar {
        let max_w = f.area().width.saturating_sub(20).max(16);
        let sidebar_w = app.sidebar_width.clamp(16, max_w);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_w), Constraint::Min(20)])
            .split(root[1]);
        render_sidebar(f, app, body[0]);
        render_main(f, app, body[1]);
    } else {
        render_main(f, app, root[1]);
    }

    render_footer(f, app, root[2]);
    render_modal(f, app);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            " STEERGLASS ",
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];

    match app.screen {
        Screen::Overview => {
            spans.push(Span::styled(
                "Overview",
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(" ({} clusters)", app.contexts.len()),
                Style::default().fg(DIM),
            ));
        }
        Screen::Resources => {
            if app.selected_contexts.is_empty() {
                spans.push(Span::styled("No cluster selected", Style::default().fg(DIM)));
            } else {
                for (i, ctx) in app.selected_contexts.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled(" + ", Style::default().fg(DIM)));
                    }
                    if let Some(env) = app.context_env(ctx) {
                        spans.push(Span::styled(
                            format!("[{}] ", env.tag()),
                            Style::default().fg(env.color()).add_modifier(Modifier::BOLD),
                        ));
                    }
                    spans.push(Span::styled(
                        ctx.as_str(),
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ));
                }
            }

            spans.push(Span::styled("  │  ", Style::default().fg(DIM)));
            spans.push(Span::styled(
                &app.active_kind.kind,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));

            if app.active_kind.namespaced {
                let ns_label = app
                    .active_namespace
                    .as_deref()
                    .unwrap_or("All Namespaces");
                spans.push(Span::styled("  ns:", Style::default().fg(DIM)));
                spans.push(Span::styled(ns_label, Style::default().fg(Color::Yellow)));
            }

            if app.filter_active || !app.filter_text.is_empty() {
                spans.push(Span::styled("  │  /", Style::default().fg(DIM)));
                spans.push(Span::styled(
                    &app.filter_text,
                    Style::default().fg(Color::Green),
                ));
                if app.filter_active {
                    spans.push(Span::styled("▌", Style::default().fg(Color::Green)));
                }
            }
        }
        Screen::Utilization => {
            spans.push(Span::styled(
                "Utilization",
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ));
            let sort_lbl = match app.utilization_sort {
                UtilizationSort::CpuDesc => "sort: CPU",
                UtilizationSort::MemDesc => "sort: Memory",
            };
            spans.push(Span::styled(format!("  ({sort_lbl})"), Style::default().fg(ACCENT)));
            if app.filter_active || !app.filter_text.is_empty() {
                spans.push(Span::styled("  │  /", Style::default().fg(DIM)));
                spans.push(Span::styled(
                    &app.filter_text,
                    Style::default().fg(Color::Green),
                ));
                if app.filter_active {
                    spans.push(Span::styled("▌", Style::default().fg(Color::Green)));
                }
            }
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM));
    let p = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(p, area);
}

fn render_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let border_color = if focused { ACCENT } else { DIM };
    let block = Block::default()
        .title(" Clusters ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let mut items = Vec::with_capacity(1 + app.contexts.len());
    let ov_active = app.screen == Screen::Overview;
    items.push(ListItem::new(Line::from(vec![
        Span::styled(
            if ov_active { "▸ " } else { "  " },
            Style::default().fg(ACCENT),
        ),
        Span::styled("0 ", Style::default().fg(DIM)),
        Span::styled(
            "Overview",
            Style::default()
                .fg(if ov_active { Color::White } else { Color::Gray })
                .add_modifier(if ov_active { Modifier::BOLD } else { Modifier::empty() }),
        ),
    ])));

    for (idx, ctx) in app.contexts.iter().enumerate() {
        let selected = app.screen == Screen::Resources
            && app.selected_contexts.contains(&ctx.name);
        let mark = if selected { "✓ " } else { "  " };
        let num = if idx < 9 {
            format!("{} ", idx + 1)
        } else {
            "  ".to_string()
        };

        let mut spans = vec![
            Span::styled(mark, Style::default().fg(ACCENT)),
            Span::styled(num, Style::default().fg(DIM)),
        ];
        if let Some(env) = app.context_env(&ctx.name) {
            spans.push(Span::styled(
                format!("{} ", env.tag()),
                Style::default().fg(env.color()).add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(
            ctx.name.as_str(),
            Style::default()
                .fg(if selected { Color::White } else { Color::Gray })
                .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() }),
        ));
        items.push(ListItem::new(Line::from(spans)));
    }

    let mut state = ListState::default();
    state.select(Some(app.sidebar_cursor));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(30, 41, 59))
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, area, &mut state);
}

fn render_main(f: &mut Frame, app: &App, area: Rect) {
    match app.screen {
        Screen::Overview => render_overview(f, app, area),
        Screen::Resources => render_resources(f, app, area),
        Screen::Utilization => render_utilization(f, app, area),
    }
}

fn render_overview(f: &mut Frame, app: &App, area: Rect) {
    let top_pct = app.overview_split_pct.clamp(20, 80);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(top_pct),
            Constraint::Percentage(100 - top_pct),
        ])
        .split(area);

    // Top: Clusters summary table
    let clusters_focused = app.focus == Focus::Main && app.overview_pane == OverviewPane::Clusters;
    let block = Block::default()
        .title(" Clusters (Enter: open, Space: multi-select, e: env, s: scan, o: pane) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if clusters_focused { ACCENT } else { DIM }));

    if app.contexts.is_empty() {
        let msg = if app.kube_errors.is_empty() {
            "No Kubernetes contexts found in ~/.kube/config or $KUBECONFIG.".to_string()
        } else {
            format!("Kubeconfig errors:\n{}", app.kube_errors.join("\n"))
        };
        f.render_widget(Paragraph::new(msg).block(block).wrap(Wrap { trim: true }), chunks[0]);
    } else {
        let header = TuiRow::new(vec![
            Cell::from("Context"),
            Cell::from("Env"),
            Cell::from("Status"),
            Cell::from("Version"),
            Cell::from("Nodes"),
            Cell::from("Pods"),
            Cell::from("Latency"),
            Cell::from("Issues"),
        ])
        .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

        let rows: Vec<TuiRow> = app
            .contexts
            .iter()
            .map(|c| {
                let env_cell = match app.context_env(&c.name) {
                    Some(e) => Cell::from(e.tag()).style(Style::default().fg(e.color()).add_modifier(Modifier::BOLD)),
                    None => Cell::from("-").style(Style::default().fg(DIM)),
                };
                let issue_count = app.issues_by_ctx.get(&c.name).map(|v| v.len()).unwrap_or(0);
                let issue_cell = if issue_count > 0 {
                    Cell::from(issue_count.to_string()).style(Style::default().fg(Color::Yellow))
                } else {
                    Cell::from("0").style(Style::default().fg(Color::Green))
                };

                match app.summaries.get(&c.name) {
                    Some(Ok(s)) => TuiRow::new(vec![
                        Cell::from(c.name.as_str()),
                        env_cell,
                        Cell::from("Connected").style(Style::default().fg(Color::Green)),
                        Cell::from(s.version.as_str()),
                        Cell::from(format!("{}/{}", s.nodes_ready, s.nodes)),
                        Cell::from(s.pods.map(|p| p.to_string()).unwrap_or_else(|| "-".into())),
                        Cell::from(format!("{} ms", s.latency_ms)),
                        issue_cell,
                    ]),
                    Some(Err(e)) => TuiRow::new(vec![
                        Cell::from(c.name.as_str()),
                        env_cell,
                        Cell::from("Error").style(Style::default().fg(Color::Red)),
                        Cell::from("-"),
                        Cell::from("-"),
                        Cell::from("-"),
                        Cell::from(truncate(e, 24)).style(Style::default().fg(Color::Red)),
                        issue_cell,
                    ]),
                    None => TuiRow::new(vec![
                        Cell::from(c.name.as_str()),
                        env_cell,
                        Cell::from("Connecting...").style(Style::default().fg(DIM)),
                        Cell::from("-"),
                        Cell::from("-"),
                        Cell::from("-"),
                        Cell::from("-"),
                        issue_cell,
                    ]),
                }
            })
            .collect();

        let widths = [
            Constraint::Percentage(24),
            Constraint::Length(7),
            Constraint::Length(13),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(8),
        ];

        let mut table_state = TableState::default();
        table_state.select(Some(app.overview_cluster_cursor));
        let table = Table::new(rows, widths)
            .header(header)
            .block(block)
            .row_highlight_style(Style::default().bg(Color::Rgb(30, 41, 59)).add_modifier(Modifier::BOLD));
        f.render_stateful_widget(table, chunks[0], &mut table_state);
    }

    // Bottom: Needs attention issues
    let issues_focused = app.focus == Focus::Main && app.overview_pane == OverviewPane::Issues;
    let issues = app.all_issues();
    let issues_block = Block::default()
        .title(format!(" Needs Attention ({}) ", issues.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if issues_focused { ACCENT } else { DIM }));

    if issues.is_empty() {
        let p = Paragraph::new("No active cluster issues detected.")
            .style(Style::default().fg(Color::Green))
            .block(issues_block);
        f.render_widget(p, chunks[1]);
    } else {
        let header = TuiRow::new(vec![
            Cell::from("Sev"),
            Cell::from("Cluster"),
            Cell::from("Kind"),
            Cell::from("Namespace"),
            Cell::from("Name"),
            Cell::from("Reason"),
        ])
        .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

        let rows: Vec<TuiRow> = issues
            .iter()
            .map(|ci| {
                let (sev_str, sev_col) = match ci.issue.severity {
                    Severity::Error => ("ERR", Color::Red),
                    Severity::Warn => ("WARN", Color::Yellow),
                };
                TuiRow::new(vec![
                    Cell::from(sev_str).style(Style::default().fg(sev_col).add_modifier(Modifier::BOLD)),
                    Cell::from(ci.context.as_str()),
                    Cell::from(ci.issue.kind.as_str()),
                    Cell::from(ci.issue.namespace.as_deref().unwrap_or("-")),
                    Cell::from(ci.issue.name.as_str()),
                    Cell::from(ci.issue.reason.as_str()),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(5),
            Constraint::Percentage(18),
            Constraint::Length(14),
            Constraint::Length(16),
            Constraint::Percentage(22),
            Constraint::Min(20),
        ];

        let mut table_state = TableState::default();
        table_state.select(Some(app.overview_issue_cursor.min(issues.len().saturating_sub(1))));
        let table = Table::new(rows, widths)
            .header(header)
            .block(issues_block)
            .row_highlight_style(Style::default().bg(Color::Rgb(30, 41, 59)).add_modifier(Modifier::BOLD));
        f.render_stateful_widget(table, chunks[1], &mut table_state);
    }
}

fn render_resources(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Main;
    let multi = app.selected_contexts.len() > 1;
    let filtered = app.filtered_rows();

    let syncing = app
        .selected_contexts
        .iter()
        .any(|c| !app.synced_ctxs.get(c).copied().unwrap_or(false));
    let sync_tag = if syncing { " [syncing...]" } else { "" };

    let title = format!(
        " {} ({}){} ",
        app.active_kind.plural,
        filtered.len(),
        sync_tag
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { ACCENT } else { DIM }));

    if !app.watch_errors.is_empty() && filtered.is_empty() {
        let errs: Vec<String> = app
            .watch_errors
            .iter()
            .map(|(c, e)| format!("{c}: {e}"))
            .collect();
        let p = Paragraph::new(errs.join("\n"))
            .style(Style::default().fg(Color::Red))
            .block(block)
            .wrap(Wrap { trim: true });
        f.render_widget(p, area);
        return;
    }

    let extra_cols = rows::columns(&app.active_kind.group, &app.active_kind.kind);
    let mut headers = Vec::new();
    let mut widths = Vec::new();

    if multi {
        headers.push(Cell::from("Cluster"));
        widths.push(Constraint::Length(18));
    }
    headers.push(Cell::from("Name"));
    widths.push(Constraint::Percentage(26));

    if app.active_kind.namespaced {
        headers.push(Cell::from("Namespace"));
        widths.push(Constraint::Length(16));
    }
    for col in &extra_cols {
        headers.push(Cell::from(*col));
        widths.push(Constraint::Min(10));
    }
    let age_header = match app.resource_sort {
        crate::app::ResourceSort::NameAsc => "Age",
        crate::app::ResourceSort::AgeNewestFirst => "Age ↓",
        crate::app::ResourceSort::AgeOldestFirst => "Age ↑",
    };
    headers.push(Cell::from(age_header));
    widths.push(Constraint::Length(7));

    let header_row = TuiRow::new(headers)
        .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

    let rows: Vec<TuiRow> = filtered
        .iter()
        .map(|tr| {
            let health_color = match tr.row.health {
                Health::Ok => Color::Green,
                Health::Warn => Color::Yellow,
                Health::Error => Color::Red,
                Health::Neutral => Color::Gray,
            };
            let mut cells = Vec::new();
            if multi {
                let env_col = app.context_env(&tr.context)
                    .map(|e| e.color())
                    .unwrap_or(Color::White);
                cells.push(Cell::from(tr.context.as_str()).style(Style::default().fg(env_col)));
            }
            cells.push(Cell::from(tr.row.name.as_str()).style(Style::default().fg(health_color)));
            if app.active_kind.namespaced {
                cells.push(Cell::from(tr.row.namespace.as_deref().unwrap_or("")));
            }
            for (idx, val) in tr.row.cells.iter().enumerate() {
                let mut text = val.clone();
                if idx == 2 && app.active_kind.kind == "Pod" {
                    if let Some(lr) = &tr.row.last_restart {
                        text = format!("{val} ({lr})");
                    }
                }
                let style = if idx == 1 && app.active_kind.kind == "Pod" {
                    Style::default().fg(health_color)
                } else {
                    Style::default()
                };
                cells.push(Cell::from(text).style(style));
            }
            let age = tr
                .row
                .created
                .as_deref()
                .map(format_age)
                .unwrap_or_else(|| "-".into());
            cells.push(Cell::from(age).style(Style::default().fg(DIM)));
            TuiRow::new(cells)
        })
        .collect();

    let mut table_state = TableState::default();
    if !filtered.is_empty() {
        table_state.select(Some(app.table_cursor.min(filtered.len() - 1)));
    }

    let table = Table::new(rows, widths)
        .header(header_row)
        .block(block)
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(30, 41, 59))
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(table, area, &mut table_state);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, is_err, at)) = &app.toast {
        if at.elapsed().as_secs() < 5 {
            let color = if *is_err { Color::Red } else { Color::Green };
            let p = Paragraph::new(Line::from(vec![
                Span::styled(
                    if *is_err { " ERROR: " } else { " OK: " },
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(msg.as_str(), Style::default().fg(Color::White)),
            ]))
            .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(DIM)));
            f.render_widget(p, area);
            return;
        }
    }

    let keys = match app.screen {
        Screen::Overview => "0 Overview  1-9 Cluster  J/K Move  e Env  u Usage  Tab Sidebar  o Pane  Enter Open  Space Multi  s Scan  : Palette  ? Help  q Quit",
        Screen::Resources => ":/Ctrl+K Palette  / Filter  u Usage  A Age-sort  n Namespace  [/] Kind  Enter/y YAML  d Describe  c Containers  l Logs  s Shell  X Debug  F Forward  D/Del Delete  ? Help  q Quit",
        Screen::Utilization => "/ Filter  o Switch Pane  c Sort CPU  m Sort Memory  r Refresh  j/k Scroll  Enter Open Pod  0/Esc Overview  ? Help  q Quit",
    };

    let p = Paragraph::new(Line::from(Span::styled(keys, Style::default().fg(DIM))))
        .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(DIM)));
    f.render_widget(p, area);
}

fn render_modal(f: &mut Frame, app: &App) {
    match &app.modal {
        Modal::None => {}
        Modal::Help => {
            let area = centered_rect(76, 85, f.area());
            f.render_widget(Clear, area);
            let lines = vec![
                Line::from(Span::styled("Steerglass TUI Shortcuts", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from("  0            Go to Multi-Cluster Overview"),
                Line::from("  1..9         Jump to Cluster N"),
                Line::from("  J / K / S-↑↓ Move cluster down / up (Sidebar or Overview, saved)"),
                Line::from("  e            Change cluster environment classification (Sidebar or Overview)"),
                Line::from("  Tab / h / l  Switch focus between Sidebar and Main"),
                Line::from("  b            Toggle Sidebar"),
                Line::from("  u            Resource utilization view (Node & Pod CPU/Memory)"),
                Line::from("  Space / m    Toggle cluster in Multi-Cluster view"),
                Line::from("  : / Ctrl+K   Command Palette (kinds, clusters, actions)"),
                Line::from("  [ / ]        Previous / Next built-in resource kind"),
                Line::from("  n            Pick Namespace"),
                Line::from("  /            Filter table (supports status:bad, status:error, status:warn)"),
                Line::from("  A (Shift+A)  Sort by Age (newest first / oldest first / name)"),
                Line::from("  j / k / ↑ /↓ Navigate rows (g/G top/bottom, PgUp/PgDn)"),
                Line::from("  Enter / y    View YAML"),
                Line::from("  d            Describe selected resource (kubectl describe)"),
                Line::from("  x / i        Decode Secret data / Interpret certs, JSON, JWT"),
                Line::from("  c (in YAML)  Copy displayed YAML/decoded/interpreted content"),
                Line::from("  z (in YAML/Logs) Toggle borderless fullscreen view for clean selection"),
                Line::from("  E            Edit YAML in $EDITOR and apply"),
                Line::from("  l            Pod Logs (/: filter, s/Ctrl+F: search, n/N: next/prev, f: follow, t: ts, w: wrap, c: container)"),
                Line::from("  c / C (Pod)  Pod Containers view (l: logs, s: shell, d/X: debug)"),
                Line::from("  s            Pod Shell (kubectl exec -it) / Overview: full scan"),
                Line::from("  X (Shift+X)  Debug container (kubectl debug on Pod or Node)"),
                Line::from("  F (Shift+F)  Port-forward (Pod or Service)"),
                Line::from("  r            Rollout Restart (Deployment/StatefulSet/DaemonSet)"),
                Line::from("  S            Scale replicas (Deployment/StatefulSet/ReplicaSet)"),
                Line::from("  c / p        Node Cordon/Uncordon  |  CronJob Suspend/Resume"),
                Line::from("  D / Del      Delete resource (PROD requires typing name)"),
                Line::from("  R            Reload kubeconfig"),
                Line::from("  Ctrl+Z       Suspend TUI (fg to resume)"),
                Line::from("  q / Ctrl+C   Quit"),
            ];
            let p = Paragraph::new(lines)
                .block(Block::default().title(" Help (Esc to close) ").borders(Borders::ALL).border_style(Style::default().fg(ACCENT)));
            f.render_widget(p, area);
        }
        Modal::CommandPalette { query, selected } => {
            let area = centered_rect(60, 60, f.area());
            f.render_widget(Clear, area);
            let block = Block::default()
                .title(" Command Palette (Enter to select, Esc to close) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(2), Constraint::Min(1)])
                .split(inner);

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("> ", Style::default().fg(ACCENT)),
                    Span::raw(query.as_str()),
                    Span::styled("▌", Style::default().fg(ACCENT)),
                ])),
                chunks[0],
            );

            let items: Vec<ListItem> = app
                .palette_items(query)
                .into_iter()
                .map(|i| ListItem::new(i.label()))
                .collect();
            let mut state = ListState::default();
            if !items.is_empty() {
                state.select(Some((*selected).min(items.len() - 1)));
            }
            let list = List::new(items)
                .highlight_style(Style::default().bg(Color::Rgb(30, 41, 59)).fg(ACCENT).add_modifier(Modifier::BOLD));
            f.render_stateful_widget(list, chunks[1], &mut state);
        }
        Modal::NamespacePicker { query, selected } => {
            let area = centered_rect(50, 55, f.area());
            f.render_widget(Clear, area);
            let block = Block::default()
                .title(" Namespace (Enter to select, Esc to close) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(2), Constraint::Min(1)])
                .split(inner);

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("ns> ", Style::default().fg(ACCENT)),
                    Span::raw(query.as_str()),
                    Span::styled("▌", Style::default().fg(ACCENT)),
                ])),
                chunks[0],
            );

            let items: Vec<ListItem> = app
                .filtered_namespaces(query)
                .into_iter()
                .map(|ns| ListItem::new(ns.unwrap_or_else(|| "All Namespaces".into())))
                .collect();
            let mut state = ListState::default();
            if !items.is_empty() {
                state.select(Some((*selected).min(items.len() - 1)));
            }
            let list = List::new(items)
                .highlight_style(Style::default().bg(Color::Rgb(30, 41, 59)).fg(ACCENT).add_modifier(Modifier::BOLD));
            f.render_stateful_widget(list, chunks[1], &mut state);
        }
        Modal::DescribeViewer {
            context,
            kind,
            name,
            lines,
            fullscreen,
            scroll,
            loading,
            error,
            ..
        } => {
            let title = format!(
                " Describe: {context} / {} / {name} (j/k scroll, c copy, z full, Esc close) ",
                kind.kind
            );
            if *fullscreen {
                let area = f.area();
                f.render_widget(Clear, area);
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Min(1)])
                    .split(area);
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        title,
                        Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD),
                    ))),
                    chunks[0],
                );
                if *loading {
                    f.render_widget(Paragraph::new("Running kubectl describe..."), chunks[1]);
                } else if let Some(err) = error {
                    f.render_widget(
                        Paragraph::new(err.as_str())
                            .style(Style::default().fg(Color::Red))
                            .wrap(Wrap { trim: false }),
                        chunks[1],
                    );
                } else {
                    let inner_h = chunks[1].height as usize;
                    let visible: Vec<Line> = lines
                        .iter()
                        .skip(*scroll)
                        .take(inner_h)
                        .map(|l| color_describe_line(l))
                        .collect();
                    f.render_widget(Paragraph::new(visible), chunks[1]);
                }
            } else {
                let area = centered_rect(88, 86, f.area());
                f.render_widget(Clear, area);
                let block = Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT));

                if *loading {
                    f.render_widget(Paragraph::new("Running kubectl describe...").block(block), area);
                } else if let Some(err) = error {
                    f.render_widget(
                        Paragraph::new(err.as_str())
                            .style(Style::default().fg(Color::Red))
                            .block(block)
                            .wrap(Wrap { trim: false }),
                        area,
                    );
                } else {
                    let inner_h = area.height.saturating_sub(2) as usize;
                    let visible: Vec<Line> = lines
                        .iter()
                        .skip(*scroll)
                        .take(inner_h)
                        .map(|l| color_describe_line(l))
                        .collect();
                    f.render_widget(Paragraph::new(visible).block(block), area);
                }
            }
        }
        Modal::YamlViewer {
            context,
            kind,
            name,
            lines,
            mode,
            fullscreen,
            scroll,
            loading,
            error,
            ..
        } => {
            let mode_tag = match mode {
                YamlViewMode::Raw => "",
                YamlViewMode::Decoded => " [DECODED]",
                YamlViewMode::Interpreted => " [INTERPRETED]",
            };
            let title = format!(
                " YAML: {context} / {} / {name}{mode_tag} (j/k scroll, x decode, i interpret, c copy, z full, E $EDITOR, Esc close) ",
                kind.kind
            );

            if *fullscreen {
                let area = f.area();
                f.render_widget(Clear, area);
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Min(1)])
                    .split(area);
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        title,
                        Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD),
                    ))),
                    chunks[0],
                );
                if *loading {
                    f.render_widget(Paragraph::new("Loading YAML..."), chunks[1]);
                } else if let Some(err) = error {
                    f.render_widget(
                        Paragraph::new(err.as_str())
                            .style(Style::default().fg(Color::Red))
                            .wrap(Wrap { trim: false }),
                        chunks[1],
                    );
                } else {
                    let inner_h = chunks[1].height as usize;
                    let visible: Vec<Line> = lines
                        .iter()
                        .skip(*scroll)
                        .take(inner_h)
                        .map(|l| color_yaml_line(l))
                        .collect();
                    f.render_widget(Paragraph::new(visible), chunks[1]);
                }
            } else {
                let area = centered_rect(88, 86, f.area());
                f.render_widget(Clear, area);
                let block = Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT));

                if *loading {
                    f.render_widget(Paragraph::new("Loading YAML...").block(block), area);
                } else if let Some(err) = error {
                    f.render_widget(
                        Paragraph::new(err.as_str())
                            .style(Style::default().fg(Color::Red))
                            .block(block)
                            .wrap(Wrap { trim: false }),
                        area,
                    );
                } else {
                    let inner_h = area.height.saturating_sub(2) as usize;
                    let visible: Vec<Line> = lines
                        .iter()
                        .skip(*scroll)
                        .take(inner_h)
                        .map(|l| color_yaml_line(l))
                        .collect();
                    f.render_widget(Paragraph::new(visible).block(block), area);
                }
            }
        }
        Modal::LogsViewer {
            context,
            pod,
            container,
            containers,
            lines,
            fullscreen,
            scroll,
            follow,
            timestamps,
            wrap,
            filter_active,
            filter_text,
            search_active,
            search_text,
            search_match,
            container_picker,
            error,
            ..
        } => {
            let c_name = container.as_deref().unwrap_or("default");
            let c_label = if containers.len() > 1 {
                let idx = container
                    .as_ref()
                    .and_then(|c| containers.iter().position(|x| x == c))
                    .map(|i| i + 1)
                    .unwrap_or(1);
                format!("{c_name} {idx}/{}", containers.len())
            } else {
                c_name.to_string()
            };
            let title = format!(
                " Logs: {context} / {pod} [{c_label}] (follow:{} ts:{} wrap:{} | /:filter s:search f:follow t:ts w:wrap c:container z:full Esc:close) ",
                if *follow { "ON" } else { "OFF" },
                if *timestamps { "ON" } else { "OFF" },
                if *wrap { "ON" } else { "OFF" }
            );
            let show_filter = *filter_active || !filter_text.is_empty();
            let show_search = *search_active || !search_text.is_empty();
            let show_bar = show_filter || show_search;
            let bar_spans = || -> Vec<Span<'_>> {
                let mut spans = Vec::new();
                if show_filter {
                    spans.push(Span::styled(" /", Style::default().fg(DIM)));
                    spans.push(Span::styled(filter_text.as_str(), Style::default().fg(Color::Green)));
                    if *filter_active {
                        spans.push(Span::styled("▌", Style::default().fg(Color::Green)));
                    }
                }
                if show_filter && show_search {
                    spans.push(Span::styled("  │ ", Style::default().fg(DIM)));
                }
                if show_search {
                    spans.push(Span::styled(" Search: ", Style::default().fg(DIM)));
                    spans.push(Span::styled(search_text.as_str(), Style::default().fg(Color::Cyan)));
                    if *search_active {
                        spans.push(Span::styled("▌", Style::default().fg(Color::Cyan)));
                    } else {
                        spans.push(Span::styled(" (n/N)", Style::default().fg(DIM)));
                    }
                }
                spans.push(Span::raw(" "));
                spans
            };
            if *fullscreen {
                let area = f.area();
                f.render_widget(Clear, area);
                let chunks = if show_bar {
                    Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
                        .split(area)
                } else {
                    Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(1), Constraint::Min(1)])
                        .split(area)
                };
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        title,
                        Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD),
                    ))),
                    chunks[0],
                );
                let body_area = if show_bar {
                    f.render_widget(Paragraph::new(Line::from(bar_spans())), chunks[1]);
                    chunks[2]
                } else {
                    chunks[1]
                };
                if let Some(err) = error {
                    f.render_widget(
                        Paragraph::new(err.as_str())
                            .style(Style::default().fg(Color::Red))
                            .wrap(Wrap { trim: false }),
                        body_area,
                    );
                } else {
                    render_logs_body(f, body_area, None, lines, *scroll, *follow, *wrap, filter_text, search_text, *search_match);
                }
            } else {
                let area = centered_rect(90, 86, f.area());
                f.render_widget(Clear, area);
                let mut block = Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT));
                if show_bar {
                    block = block.title_bottom(Line::from(bar_spans()));
                }

                if let Some(err) = error {
                    f.render_widget(
                        Paragraph::new(err.as_str())
                            .style(Style::default().fg(Color::Red))
                            .block(block)
                            .wrap(Wrap { trim: false }),
                        area,
                    );
                } else {
                    render_logs_body(f, area, Some(block), lines, *scroll, *follow, *wrap, filter_text, search_text, *search_match);
                }
            }
            if let Some(sel) = container_picker {
                let height = (containers.len() as u16 + 2).min(f.area().height.saturating_sub(4)).max(4);
                let width = 48u16.min(f.area().width.saturating_sub(4)).max(20);
                let popup = Rect {
                    x: f.area().x + f.area().width.saturating_sub(width) / 2,
                    y: f.area().y + f.area().height.saturating_sub(height) / 2,
                    width,
                    height,
                };
                f.render_widget(Clear, popup);
                let items: Vec<ListItem> = containers
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let active = container.as_deref() == Some(c.as_str());
                        let num = if i < 9 { format!("{} ", i + 1) } else { "  ".into() };
                        let suffix = if active { " (active)" } else { "" };
                        let style = if i == *sel {
                            Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)
                        } else if active {
                            Style::default().fg(ACCENT)
                        } else {
                            Style::default()
                        };
                        ListItem::new(Line::from(Span::styled(format!(" {num}{c}{suffix}"), style)))
                    })
                    .collect();
                let list = List::new(items).block(
                    Block::default()
                        .title(" Select Container (↑/↓, 1-9, Enter, Esc) ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(ACCENT)),
                );
                f.render_widget(list, popup);
            }
        }
        Modal::Confirm { kind, typed } => {
            let area = centered_rect(56, 30, f.area());
            f.render_widget(Clear, area);
            let (title, body, border) = match kind {
                ConfirmKind::Delete {
                    context,
                    kind,
                    name,
                    is_prod: true,
                    ..
                } => (
                    " PRODUCTION DELETE ",
                    vec![
                        Line::from(Span::styled(
                            format!("Cluster '{context}' is PRODUCTION!"),
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        )),
                        Line::from(format!("Delete {} '{name}'?", kind.kind)),
                        Line::from(""),
                        Line::from(format!("Type '{name}' and press Enter to confirm:")),
                        Line::from(Span::styled(
                            format!("> {typed}▌"),
                            Style::default().fg(Color::Yellow),
                        )),
                    ],
                    Color::Red,
                ),
                ConfirmKind::Delete {
                    context,
                    kind,
                    name,
                    is_prod: false,
                    ..
                } => (
                    " Confirm Delete ",
                    vec![
                        Line::from(format!("Delete {} '{name}' in '{context}'?", kind.kind)),
                        Line::from(""),
                        Line::from("Press 'y' or Enter to confirm, Esc to cancel."),
                    ],
                    Color::Yellow,
                ),
                ConfirmKind::Restart {
                    context,
                    kind,
                    name,
                    ..
                } => (
                    " Confirm Rollout Restart ",
                    vec![
                        Line::from(format!("Restart {} '{name}' in '{context}'?", kind.kind)),
                        Line::from(""),
                        Line::from("Press 'y' or Enter to confirm, Esc to cancel."),
                    ],
                    ACCENT,
                ),
                ConfirmKind::Cordon {
                    context,
                    name,
                    cordon,
                } => {
                    let verb = if *cordon { "Cordon" } else { "Uncordon" };
                    (
                        " Confirm Node Action ",
                        vec![
                            Line::from(format!("{verb} node '{name}' in '{context}'?")),
                            Line::from(""),
                            Line::from("Press 'y' or Enter to confirm, Esc to cancel."),
                        ],
                        ACCENT,
                    )
                }
                ConfirmKind::SuspendCronJob {
                    context,
                    name,
                    suspend,
                    ..
                } => {
                    let verb = if *suspend { "Suspend" } else { "Resume" };
                    (
                        " Confirm CronJob Action ",
                        vec![
                            Line::from(format!("{verb} CronJob '{name}' in '{context}'?")),
                            Line::from(""),
                            Line::from("Press 'y' or Enter to confirm, Esc to cancel."),
                        ],
                        ACCENT,
                    )
                }
            };
            let p = Paragraph::new(body)
                .block(Block::default().title(title).borders(Borders::ALL).border_style(Style::default().fg(border)))
                .wrap(Wrap { trim: true });
            f.render_widget(p, area);
        }
        Modal::ScalePrompt {
            context,
            kind,
            name,
            input,
            ..
        } => {
            let area = centered_rect(48, 25, f.area());
            f.render_widget(Clear, area);
            let lines = vec![
                Line::from(format!("Scale {} '{name}' in '{context}'", kind.kind)),
                Line::from(""),
                Line::from(vec![
                    Span::raw("Replicas: "),
                    Span::styled(format!("{input}▌"), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(""),
                Line::from(Span::styled("Enter to apply, Esc to cancel", Style::default().fg(DIM))),
            ];
            let p = Paragraph::new(lines)
                .block(Block::default().title(" Scale ").borders(Borders::ALL).border_style(Style::default().fg(ACCENT)));
            f.render_widget(p, area);
        }
        Modal::ContainerPicker {
            pod,
            purpose,
            containers,
            selected,
            loading,
            ..
        } => {
            let what = match purpose {
                ContainerPurpose::Logs => "Logs",
                ContainerPurpose::Shell => "Shell",
            };
            let height = if *loading {
                5
            } else {
                (containers.len() as u16 + 2).min(f.area().height.saturating_sub(4)).max(5)
            };
            let width = 56u16.min(f.area().width.saturating_sub(4)).max(24);
            let area = Rect {
                x: f.area().x + f.area().width.saturating_sub(width) / 2,
                y: f.area().y + f.area().height.saturating_sub(height) / 2,
                width,
                height,
            };
            f.render_widget(Clear, area);
            let block = Block::default()
                .title(format!(" Select Container for {what}: {pod} (↑/↓, 1-9, Enter, Esc) "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));
            if *loading {
                f.render_widget(Paragraph::new(" Loading containers...").block(block), area);
            } else {
                let items: Vec<ListItem> = containers
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let num = if i < 9 { format!("{} ", i + 1) } else { "  ".into() };
                        let style = if i == *selected {
                            Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        ListItem::new(Line::from(Span::styled(format!(" {num}{c}"), style)))
                    })
                    .collect();
                f.render_widget(List::new(items).block(block), area);
            }
        }
        Modal::ContainersView {
            context,
            pod,
            containers,
            selected,
            loading,
            error,
            ..
        } => {
            let area = centered_rect(90, 72, f.area());
            f.render_widget(Clear, area);
            let block = Block::default()
                .title(format!(
                    " Containers: {context} / {pod} (↑/↓ select, l/Enter logs, s shell, d/X debug, r refresh, Esc close) "
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));
            if *loading {
                f.render_widget(Paragraph::new(" Loading containers...").block(block), area);
            } else if let Some(err) = error {
                f.render_widget(
                    Paragraph::new(err.as_str())
                        .style(Style::default().fg(Color::Red))
                        .block(block)
                        .wrap(Wrap { trim: false }),
                    area,
                );
            } else {
                let header = TuiRow::new(vec![
                    Cell::from("NAME"),
                    Cell::from("TYPE"),
                    Cell::from("STATE"),
                    Cell::from("READY"),
                    Cell::from("RESTARTS"),
                    Cell::from("PORTS"),
                    Cell::from("IMAGE"),
                ])
                .style(Style::default().fg(DIM).add_modifier(Modifier::BOLD));
                let rows: Vec<TuiRow> = containers
                    .iter()
                    .map(|c| {
                        let st_color = if c.state == "Running" {
                            Color::Green
                        } else if c.state.contains("CrashLoop")
                            || c.state.contains("Error")
                            || c.state.contains("Err")
                            || c.state.contains("Failed")
                        {
                            Color::Red
                        } else if c.state.starts_with("Waiting") {
                            Color::Yellow
                        } else {
                            DIM
                        };
                        TuiRow::new(vec![
                            Cell::from(c.name.as_str()),
                            Cell::from(c.kind.as_str()).style(Style::default().fg(DIM)),
                            Cell::from(c.state.as_str()).style(Style::default().fg(st_color)),
                            Cell::from(if c.ready { "true" } else { "false" }),
                            Cell::from(c.restarts.to_string()),
                            Cell::from(c.ports.as_str()),
                            Cell::from(c.image.as_str()),
                        ])
                    })
                    .collect();
                let widths = [
                    Constraint::Percentage(18),
                    Constraint::Length(10),
                    Constraint::Percentage(20),
                    Constraint::Length(6),
                    Constraint::Length(9),
                    Constraint::Percentage(14),
                    Constraint::Percentage(26),
                ];
                let table = Table::new(rows, widths)
                    .header(header)
                    .block(block)
                    .row_highlight_style(
                        Style::default()
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    );
                let mut state = TableState::default();
                if !containers.is_empty() {
                    state.select(Some((*selected).min(containers.len() - 1)));
                }
                f.render_stateful_widget(table, area, &mut state);
            }
        }
        Modal::EnvPicker { context, selected } => {
            let area = centered_rect(54, 38, f.area());
            f.render_widget(Clear, area);
            let block = Block::default()
                .title(format!(" Environment: {} (Enter to save, Esc to cancel) ", truncate(context, 32)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(2), Constraint::Min(1)])
                .split(inner);

            let auto_str = match detect_env(context) {
                Some(e) => e.tag(),
                None => "None",
            };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Auto-detected: ", Style::default().fg(DIM)),
                    Span::styled(auto_str, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                ])),
                chunks[0],
            );

            let items: Vec<ListItem> = EnvChoice::ALL
                .iter()
                .map(|&choice| {
                    let style = match choice.env().or_else(|| {
                        if choice == EnvChoice::Auto {
                            detect_env(context)
                        } else {
                            None
                        }
                    }) {
                        Some(env) => Style::default().fg(env.color()),
                        None => Style::default().fg(Color::Gray),
                    };
                    let label = if choice == EnvChoice::Auto {
                        format!("Auto ({auto_str})")
                    } else {
                        choice.label().to_string()
                    };
                    ListItem::new(Span::styled(label, style))
                })
                .collect();
            let mut state = ListState::default();
            state.select(Some((*selected).min(EnvChoice::ALL.len().saturating_sub(1))));
            let list = List::new(items).highlight_style(
                Style::default()
                    .bg(Color::Rgb(30, 41, 59))
                    .add_modifier(Modifier::BOLD),
            );
            f.render_stateful_widget(list, chunks[1], &mut state);
        }
        Modal::DebugPrompt {
            name,
            is_node,
            selected_preset,
            custom_image,
            containers,
            selected_target,
            focus_target,
            ..
        } => {
            let area = centered_rect(58, if *is_node { 36 } else { 50 }, f.area());
            f.render_widget(Clear, area);
            let kind_label = if *is_node { "Node" } else { "Pod" };
            let block = Block::default()
                .title(format!(" Debug {kind_label}: {} ", truncate(name, 32)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let mut constraints = vec![Constraint::Min(1)];
            if !*is_node {
                constraints.push(Constraint::Length(5));
            }
            constraints.push(Constraint::Length(2));
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(inner);

            let mut items: Vec<ListItem> = crate::app::DEBUG_IMAGES
                .iter()
                .map(|img| ListItem::new(*img))
                .collect();
            let custom_label = if *selected_preset == crate::app::DEBUG_IMAGES.len() && !*focus_target {
                format!("Custom: {custom_image}▌")
            } else if custom_image.is_empty() {
                "Custom image…".to_string()
            } else {
                format!("Custom: {custom_image}")
            };
            items.push(ListItem::new(custom_label));

            let mut state = ListState::default();
            state.select(Some((*selected_preset).min(items.len().saturating_sub(1))));
            let list = List::new(items).highlight_style(
                Style::default()
                    .bg(Color::Rgb(30, 41, 59))
                    .fg(if *focus_target { Color::Gray } else { ACCENT })
                    .add_modifier(Modifier::BOLD),
            );
            f.render_stateful_widget(list, chunks[0], &mut state);

            let hint_idx = if !*is_node {
                let mut tgt_items = vec![ListItem::new("(none — no --target)")];
                for c in containers {
                    tgt_items.push(ListItem::new(c.as_str()));
                }
                let mut tgt_state = ListState::default();
                tgt_state.select(Some((*selected_target).min(tgt_items.len().saturating_sub(1))));
                let tgt_block = Block::default()
                    .title(" Target container ")
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(if *focus_target { ACCENT } else { DIM }));
                let tgt_list = List::new(tgt_items).block(tgt_block).highlight_style(
                    Style::default()
                        .bg(Color::Rgb(30, 41, 59))
                        .fg(if *focus_target { ACCENT } else { Color::Gray })
                        .add_modifier(Modifier::BOLD),
                );
                f.render_stateful_widget(tgt_list, chunks[1], &mut tgt_state);
                2
            } else {
                1
            };

            let hint = if *is_node {
                "↑/↓ Image  Enter Start kubectl debug  Esc Cancel"
            } else {
                "↑/↓ Image  Tab Target container  Enter Start kubectl debug  Esc Cancel"
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(DIM)))),
                chunks[hint_idx],
            );
        }
        Modal::PortForwardPrompt {
            kind,
            namespace,
            name,
            input,
            ports,
            ports_loading,
            selected_port,
            active,
            selected_active,
            focus_active,
            ..
        } => {
            let area = centered_rect(64, 54, f.area());
            f.render_widget(Clear, area);
            let block = Block::default()
                .title(format!(" Port-Forward {kind}: {namespace}/{} ", truncate(name, 28)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Percentage(50),
                    Constraint::Min(3),
                    Constraint::Length(1),
                ])
                .split(inner);

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        "Port (port | local:remote | addr:local:remote): ",
                        Style::default().fg(DIM),
                    ),
                    Span::styled(
                        format!("{input}▌"),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                ])),
                chunks[0],
            );

            let ports_block = Block::default()
                .title(" Available Ports ")
                .borders(Borders::TOP)
                .border_style(Style::default().fg(if !*focus_active { ACCENT } else { DIM }));
            if *ports_loading {
                f.render_widget(
                    Paragraph::new("Loading ports...")
                        .style(Style::default().fg(DIM))
                        .block(ports_block),
                    chunks[1],
                );
            } else if ports.is_empty() {
                f.render_widget(
                    Paragraph::new("No declared ports (type a port number above).")
                        .style(Style::default().fg(DIM))
                        .block(ports_block),
                    chunks[1],
                );
            } else {
                let items: Vec<ListItem> = ports
                    .iter()
                    .map(|p| {
                        let name_part = p.name.as_deref().unwrap_or("-");
                        let detail_part = p.detail.as_deref().unwrap_or("");
                        let suffix = if detail_part.is_empty() {
                            String::new()
                        } else {
                            format!("  ({detail_part})")
                        };
                        ListItem::new(format!(
                            "{}/{}  {}{suffix}",
                            p.port, p.protocol, name_part
                        ))
                    })
                    .collect();
                let mut state = ListState::default();
                state.select(Some((*selected_port).min(items.len().saturating_sub(1))));
                let list = List::new(items)
                    .block(ports_block)
                    .highlight_style(
                        Style::default()
                            .bg(Color::Rgb(30, 41, 59))
                            .fg(if !*focus_active { ACCENT } else { Color::Gray })
                            .add_modifier(Modifier::BOLD),
                    );
                f.render_stateful_widget(list, chunks[1], &mut state);
            }

            let active_block = Block::default()
                .title(" Active Forwards ")
                .borders(Borders::TOP)
                .border_style(Style::default().fg(if *focus_active { ACCENT } else { DIM }));
            if active.is_empty() {
                f.render_widget(
                    Paragraph::new("No active port-forwards.")
                        .style(Style::default().fg(DIM))
                        .block(active_block),
                    chunks[2],
                );
            } else {
                let items: Vec<ListItem> = active
                    .iter()
                    .map(|fw| {
                        ListItem::new(format!(
                            "localhost:{} -> {}/{}:{} ({})",
                            fw.local_port, fw.namespace, fw.target, fw.remote_port, fw.context
                        ))
                    })
                    .collect();
                let mut state = ListState::default();
                state.select(Some((*selected_active).min(items.len().saturating_sub(1))));
                let list = List::new(items)
                    .block(active_block)
                    .highlight_style(
                        Style::default()
                            .bg(Color::Rgb(30, 41, 59))
                            .fg(if *focus_active { ACCENT } else { Color::Gray })
                            .add_modifier(Modifier::BOLD),
                    );
                f.render_stateful_widget(list, chunks[2], &mut state);
            }

            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "↑/↓ Select  Enter Start  Tab Switch list  Del Stop active  Esc Cancel",
                    Style::default().fg(DIM),
                ))),
                chunks[3],
            );
        }
    }
}

fn color_yaml_line(line: &str) -> Line<'_> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return Line::from(Span::styled(line, Style::default().fg(DIM)));
    }
    if let Some((k, v)) = line.split_once(':') {
        return Line::from(vec![
            Span::styled(k, Style::default().fg(ACCENT)),
            Span::styled(":", Style::default().fg(DIM)),
            Span::styled(v, Style::default().fg(Color::White)),
        ]);
    }
    Line::from(line)
}

fn render_utilization(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let multi = app.selected_contexts.len() > 1;
    let loading = app
        .selected_contexts
        .iter()
        .any(|c| app.utilization_loading.get(c).copied().unwrap_or(false));
    let no_metrics = !loading
        && app
            .selected_contexts
            .iter()
            .any(|c| matches!(app.utilization_by_ctx.get(c), Some(Ok(u)) if !u.metrics_available));
    let status_note = if loading {
        " [loading...]"
    } else if no_metrics {
        " [metrics-server unavailable: showing requests/allocatable]"
    } else {
        ""
    };

    // Top pane: Nodes utilization
    let nodes_focused = app.focus == Focus::Main && app.utilization_pane == UtilizationPane::Nodes;
    let nodes = app.utilization_nodes();
    let nodes_block = Block::default()
        .title(format!(
            " Nodes Utilization ({}){status_note} ",
            nodes.len()
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if nodes_focused { ACCENT } else { DIM }));

    if nodes.is_empty() {
        let msg = if loading {
            "Fetching node utilization..."
        } else {
            "No node utilization data. Press 'r' to refresh."
        };
        f.render_widget(
            Paragraph::new(msg).style(Style::default().fg(DIM)).block(nodes_block),
            chunks[0],
        );
    } else {
        let mut hdr = Vec::new();
        if multi {
            hdr.push(Cell::from("Context"));
        }
        hdr.extend([
            Cell::from("Node"),
            Cell::from("CPU"),
            Cell::from("CPU Req"),
            Cell::from("Memory"),
            Cell::from("Mem Req"),
            Cell::from("Pods"),
        ]);
        let header = TuiRow::new(hdr).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

        let rows: Vec<TuiRow> = nodes
            .iter()
            .map(|(ctx, n)| {
                let cpu_cell = match n.cpu_used {
                    Some(u) => {
                        let (bar, col) = usage_bar(u, n.cpu_alloc, 8);
                        Cell::from(format!("{bar} {}/{}", format_cpu(u), format_cpu(n.cpu_alloc)))
                            .style(Style::default().fg(col))
                    }
                    None => Cell::from(format!("- / {}", format_cpu(n.cpu_alloc)))
                        .style(Style::default().fg(DIM)),
                };
                let mem_cell = match n.mem_used {
                    Some(u) => {
                        let (bar, col) = usage_bar(u, n.mem_alloc, 8);
                        Cell::from(format!("{bar} {}/{}", format_bytes(u), format_bytes(n.mem_alloc)))
                            .style(Style::default().fg(col))
                    }
                    None => Cell::from(format!("- / {}", format_bytes(n.mem_alloc)))
                        .style(Style::default().fg(DIM)),
                };
                let mut cells = Vec::new();
                if multi {
                    cells.push(Cell::from(ctx.as_str()));
                }
                cells.extend([
                    Cell::from(n.name.as_str()),
                    cpu_cell,
                    Cell::from(format_cpu(n.cpu_req)),
                    mem_cell,
                    Cell::from(format_bytes(n.mem_req)),
                    Cell::from(n.pods.to_string()),
                ]);
                TuiRow::new(cells)
            })
            .collect();

        let mut widths = Vec::new();
        if multi {
            widths.push(Constraint::Length(18));
        }
        widths.extend([
            Constraint::Min(18),
            Constraint::Length(24),
            Constraint::Length(9),
            Constraint::Length(26),
            Constraint::Length(10),
            Constraint::Length(5),
        ]);

        let mut state = TableState::default();
        state.select(Some(app.utilization_node_cursor.min(nodes.len().saturating_sub(1))));
        let table = Table::new(rows, widths)
            .header(header)
            .block(nodes_block)
            .row_highlight_style(
                Style::default()
                    .bg(Color::Rgb(30, 41, 59))
                    .add_modifier(Modifier::BOLD),
            );
        f.render_stateful_widget(table, chunks[0], &mut state);
    }

    // Bottom pane: Pods utilization
    let pods_focused = app.focus == Focus::Main && app.utilization_pane == UtilizationPane::Pods;
    let pods = app.utilization_pods();
    let pods_block = Block::default()
        .title(format!(" Pods Utilization ({}) ", pods.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if pods_focused { ACCENT } else { DIM }));

    if pods.is_empty() {
        let msg = if loading {
            "Fetching pod utilization..."
        } else {
            "No pod utilization data. Press 'r' to refresh."
        };
        f.render_widget(
            Paragraph::new(msg).style(Style::default().fg(DIM)).block(pods_block),
            chunks[1],
        );
    } else {
        let mut hdr = Vec::new();
        if multi {
            hdr.push(Cell::from("Context"));
        }
        hdr.extend([
            Cell::from("Namespace"),
            Cell::from("Pod"),
            Cell::from("CPU Used"),
            Cell::from("CPU Req/Lim"),
            Cell::from("Mem Used"),
            Cell::from("Mem Req/Lim"),
        ]);
        let header = TuiRow::new(hdr).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

        let rows: Vec<TuiRow> = pods
            .iter()
            .map(|(ctx, p)| {
                let cpu_used_cell = match p.cpu_used {
                    Some(u) => {
                        let ref_cap = p.cpu_lim.unwrap_or(p.cpu_req);
                        let col = if ref_cap > 0.0 && u / ref_cap >= 0.9 {
                            Color::Red
                        } else if ref_cap > 0.0 && u / ref_cap >= 0.7 {
                            Color::Yellow
                        } else {
                            Color::Green
                        };
                        Cell::from(format_cpu(u)).style(Style::default().fg(col))
                    }
                    None => Cell::from("-").style(Style::default().fg(DIM)),
                };
                let mem_used_cell = match p.mem_used {
                    Some(u) => {
                        let ref_cap = p.mem_lim.unwrap_or(p.mem_req);
                        let col = if ref_cap > 0.0 && u / ref_cap >= 0.9 {
                            Color::Red
                        } else if ref_cap > 0.0 && u / ref_cap >= 0.7 {
                            Color::Yellow
                        } else {
                            Color::Green
                        };
                        Cell::from(format_bytes(u)).style(Style::default().fg(col))
                    }
                    None => Cell::from("-").style(Style::default().fg(DIM)),
                };
                let cpu_rl = format!(
                    "{}/{}",
                    format_cpu(p.cpu_req),
                    p.cpu_lim.map(format_cpu).unwrap_or_else(|| "-".into())
                );
                let mem_rl = format!(
                    "{}/{}",
                    format_bytes(p.mem_req),
                    p.mem_lim.map(format_bytes).unwrap_or_else(|| "-".into())
                );
                let mut cells = Vec::new();
                if multi {
                    cells.push(Cell::from(ctx.as_str()));
                }
                cells.extend([
                    Cell::from(p.namespace.as_str()),
                    Cell::from(p.name.as_str()),
                    cpu_used_cell,
                    Cell::from(cpu_rl),
                    mem_used_cell,
                    Cell::from(mem_rl),
                ]);
                TuiRow::new(cells)
            })
            .collect();

        let mut widths = Vec::new();
        if multi {
            widths.push(Constraint::Length(18));
        }
        widths.extend([
            Constraint::Length(16),
            Constraint::Min(22),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(16),
        ]);

        let mut state = TableState::default();
        state.select(Some(app.utilization_pod_cursor.min(pods.len().saturating_sub(1))));
        let table = Table::new(rows, widths)
            .header(header)
            .block(pods_block)
            .row_highlight_style(
                Style::default()
                    .bg(Color::Rgb(30, 41, 59))
                    .add_modifier(Modifier::BOLD),
            );
        f.render_stateful_widget(table, chunks[1], &mut state);
    }
}

fn format_cpu(cores: f64) -> String {
    if cores <= 0.0 {
        "0m".into()
    } else if cores < 1.0 {
        format!("{}m", (cores * 1000.0).round() as u64)
    } else {
        format!("{cores:.2}")
    }
}

fn format_bytes(bytes: f64) -> String {
    const GI: f64 = 1024.0 * 1024.0 * 1024.0;
    const MI: f64 = 1024.0 * 1024.0;
    const KI: f64 = 1024.0;
    if bytes <= 0.0 {
        "0".into()
    } else if bytes >= GI {
        format!("{:.1}Gi", bytes / GI)
    } else if bytes >= MI {
        format!("{:.0}Mi", bytes / MI)
    } else if bytes >= KI {
        format!("{:.0}Ki", bytes / KI)
    } else {
        format!("{:.0}B", bytes)
    }
}

fn usage_bar(used: f64, total: f64, width: usize) -> (String, Color) {
    if total <= 0.0 {
        return (format!("[{}]", "░".repeat(width)), DIM);
    }
    let ratio = (used / total).clamp(0.0, 1.0);
    let pct = ((used / total) * 100.0).round() as u64;
    let filled = ((ratio * width as f64).round() as usize).min(width);
    let bar = format!("[{}{}] {:>2}%", "█".repeat(filled), "░".repeat(width - filled), pct);
    let color = if ratio >= 0.9 {
        Color::Red
    } else if ratio >= 0.7 {
        Color::Yellow
    } else {
        Color::Green
    };
    (bar, color)
}

fn format_age(iso: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return "-".into();
    };
    let secs = (chrono::Utc::now() - dt.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[allow(clippy::too_many_arguments)]
fn render_logs_body(
    f: &mut Frame,
    area: Rect,
    block: Option<Block>,
    lines: &[String],
    scroll: usize,
    follow: bool,
    wrap: bool,
    filter_text: &str,
    search_text: &str,
    search_match: Option<usize>,
) {
    let inner_h = if block.is_some() { area.height.saturating_sub(2) } else { area.height } as usize;
    let inner_w = if block.is_some() { area.width.saturating_sub(2) } else { area.width };
    if inner_h == 0 || inner_w == 0 {
        if let Some(b) = block {
            f.render_widget(b, area);
        }
        return;
    }

    let fq = filter_text.trim().to_lowercase();
    let sq = search_text.trim().to_lowercase();
    let matched: Vec<&str> = if fq.is_empty() {
        lines.iter().map(|s| s.as_str()).collect()
    } else {
        lines
            .iter()
            .filter(|l| l.to_lowercase().contains(&fq))
            .map(|s| s.as_str())
            .collect()
    };

    let render_line = |idx: usize, l: &str| -> Line<'static> {
        let is_current = search_match == Some(idx);
        if !sq.is_empty() {
            let bg = if is_current { Color::LightCyan } else { Color::Cyan };
            highlight_log_match(l, &sq, bg)
        } else if !fq.is_empty() {
            highlight_log_match(l, &fq, Color::Yellow)
        } else {
            Line::from(l.to_string())
        }
    };

    let mut p = if wrap {
        if follow {
            let mut total_rows = 0usize;
            let mut start = matched.len();
            for (idx, l) in matched.iter().enumerate().rev() {
                let rows = wrapped_line_rows(l, inner_w as usize);
                total_rows += rows;
                start = idx;
                if total_rows >= inner_h {
                    break;
                }
            }
            let skip_rows = total_rows.saturating_sub(inner_h) as u16;
            let visible: Vec<Line> = matched[start..]
                .iter()
                .enumerate()
                .map(|(offset, l)| render_line(start + offset, l))
                .collect();
            Paragraph::new(visible).wrap(Wrap { trim: false }).scroll((skip_rows, 0))
        } else {
            let start = scroll.min(matched.len().saturating_sub(1));
            let visible: Vec<Line> = matched
                .iter()
                .enumerate()
                .skip(start)
                .take(inner_h)
                .map(|(idx, l)| render_line(idx, l))
                .collect();
            Paragraph::new(visible).wrap(Wrap { trim: false })
        }
    } else {
        let start = if follow {
            matched.len().saturating_sub(inner_h)
        } else {
            scroll.min(matched.len().saturating_sub(1))
        };
        let visible: Vec<Line> = matched
            .iter()
            .enumerate()
            .skip(start)
            .take(inner_h)
            .map(|(idx, l)| render_line(idx, l))
            .collect();
        Paragraph::new(visible)
    };

    if let Some(b) = block {
        p = p.block(b);
    }
    f.render_widget(p, area);
}

fn highlight_log_match(line: &str, query_lower: &str, bg: Color) -> Line<'static> {
    let lower = line.to_lowercase();
    if lower.len() != line.len() {
        return Line::from(line.to_string());
    }
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find(query_lower) {
        let start = pos + rel;
        let end = start + query_lower.len();
        if start > pos {
            spans.push(Span::raw(line[pos..start].to_string()));
        }
        spans.push(Span::styled(
            line[start..end].to_string(),
            Style::default().fg(Color::Black).bg(bg),
        ));
        pos = end;
    }
    if pos < line.len() {
        spans.push(Span::raw(line[pos..].to_string()));
    }
    Line::from(spans)
}

fn wrapped_line_rows(s: &str, width: usize) -> usize {
    if width == 0 || s.is_empty() {
        return 1;
    }
    let mut rows = 1usize;
    let mut col = 0usize;
    for word in s.split_inclusive(' ') {
        let w = Line::from(word).width();
        if col + w <= width {
            col += w;
        } else if w <= width {
            rows += 1;
            col = w;
        } else {
            let mut buf = [0u8; 4];
            for ch in word.chars() {
                let cw = Line::from(ch.encode_utf8(&mut buf) as &str).width();
                if col + cw > width && col > 0 {
                    rows += 1;
                    col = 0;
                }
                col += cw;
            }
        }
    }
    rows
}

fn color_describe_line(line: &str) -> Line<'static> {
    if line.contains("Warning") {
        return Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Yellow)));
    }
    if line.contains("Failed") || line.contains("Error") || line.contains("BackOff") {
        return Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Red)));
    }
    if let Some((key, rest)) = line.split_once(':') {
        if !key.trim().is_empty() && !key.starts_with(' ') {
            return Line::from(vec![
                Span::styled(format!("{key}:"), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::raw(rest.to_string()),
            ]);
        }
    }
    Line::from(line.to_string())
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
