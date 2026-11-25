use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row as TuiRow, Table,
    TableState, Wrap,
};

use crate::app::{App, ConfirmKind, Focus, Modal, OverviewPane, Screen};
use crate::core::health::Severity;
use crate::core::rows::{self, Health};
use crate::env::detect_env;

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
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Min(30)])
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
                    if let Some(env) = detect_env(ctx) {
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
        if let Some(env) = detect_env(&ctx.name) {
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
    }
}

fn render_overview(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(area);

    // Top: Clusters summary table
    let clusters_focused = app.focus == Focus::Main && app.overview_pane == OverviewPane::Clusters;
    let block = Block::default()
        .title(" Clusters (Enter: open, Space: multi-select, s: scan all kinds, o: switch pane) ")
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
                let env_cell = match detect_env(&c.name) {
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
    headers.push(Cell::from("Age"));
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
                let env_col = detect_env(&tr.context)
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
        Screen::Overview => "0 Overview  1-9 Cluster  Tab Sidebar  o Switch Pane  Enter Open  Space Multi-select  s Scan  : Palette  ? Help  q Quit",
        Screen::Resources => ":/Ctrl+K Palette  / Filter  n Namespace  [/] Kind  Enter/y YAML  l Logs  s Shell  E Edit  r Restart  S Scale  d Delete  ? Help  q Quit",
    };

    let p = Paragraph::new(Line::from(Span::styled(keys, Style::default().fg(DIM))))
        .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(DIM)));
    f.render_widget(p, area);
}

fn render_modal(f: &mut Frame, app: &App) {
    match &app.modal {
        Modal::None => {}
        Modal::Help => {
            let area = centered_rect(72, 75, f.area());
            f.render_widget(Clear, area);
            let lines = vec![
                Line::from(Span::styled("Steerglass TUI Shortcuts", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from("  0            Go to Multi-Cluster Overview"),
                Line::from("  1..9         Jump to Cluster N"),
                Line::from("  Tab / h / l  Switch focus between Sidebar and Main"),
                Line::from("  b            Toggle Sidebar"),
                Line::from("  Space / m    Toggle cluster in Multi-Cluster view"),
                Line::from("  : / Ctrl+K   Command Palette (kinds, clusters, actions)"),
                Line::from("  [ / ]        Previous / Next built-in resource kind"),
                Line::from("  n            Pick Namespace"),
                Line::from("  /            Filter table (supports status:bad, status:error, status:warn)"),
                Line::from("  j / k / ↑ /↓ Navigate rows (g/G top/bottom, PgUp/PgDn)"),
                Line::from("  Enter / y    View YAML"),
                Line::from("  E            Edit YAML in $EDITOR and apply"),
                Line::from("  l            Pod Logs (f: follow, t: timestamps, c: container)"),
                Line::from("  s            Pod Shell (kubectl exec -it) / Overview: full scan"),
                Line::from("  r            Rollout Restart (Deployment/StatefulSet/DaemonSet)"),
                Line::from("  S            Scale replicas (Deployment/StatefulSet/ReplicaSet)"),
                Line::from("  c / p        Node Cordon/Uncordon  |  CronJob Suspend/Resume"),
                Line::from("  d / Del      Delete resource (PROD requires typing name)"),
                Line::from("  R            Reload kubeconfig"),
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
        Modal::YamlViewer {
            context,
            kind,
            name,
            lines,
            scroll,
            loading,
            error,
            ..
        } => {
            let area = centered_rect(88, 86, f.area());
            f.render_widget(Clear, area);
            let title = format!(" YAML: {context} / {} / {name} (j/k scroll, E $EDITOR, Esc close) ", kind.kind);
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
        Modal::LogsViewer {
            context,
            pod,
            container,
            lines,
            scroll,
            follow,
            timestamps,
            error,
            ..
        } => {
            let area = centered_rect(90, 86, f.area());
            f.render_widget(Clear, area);
            let c_name = container.as_deref().unwrap_or("default");
            let title = format!(
                " Logs: {context} / {pod} [{c_name}] (follow:{} ts:{} | f:follow t:ts c:container Esc:close) ",
                if *follow { "ON" } else { "OFF" },
                if *timestamps { "ON" } else { "OFF" }
            );
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));

            if let Some(err) = error {
                f.render_widget(
                    Paragraph::new(err.as_str())
                        .style(Style::default().fg(Color::Red))
                        .block(block)
                        .wrap(Wrap { trim: false }),
                    area,
                );
            } else {
                let inner_h = area.height.saturating_sub(2) as usize;
                let start = if *follow {
                    lines.len().saturating_sub(inner_h)
                } else {
                    (*scroll).min(lines.len().saturating_sub(1))
                };
                let visible: Vec<Line> = lines
                    .iter()
                    .skip(start)
                    .take(inner_h)
                    .map(|l| Line::from(l.as_str()))
                    .collect();
                f.render_widget(Paragraph::new(visible).block(block), area);
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
