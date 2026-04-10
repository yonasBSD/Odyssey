//! Viewer overlay widget: sessions, skills, models, and themes list panels.

use crate::app::{App, ViewerKind};
use crate::ui::theme::AVAILABLE_THEMES;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

/// Draw the full-area viewer overlay.
pub fn draw_viewer(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let Some(kind) = app.viewer else {
        return;
    };

    let (title, lines) = match kind {
        ViewerKind::Agents => (" Agents ", render_agent_lines(app)),
        ViewerKind::Bundles => (" Bundles ", render_bundle_lines(app)),
        ViewerKind::Sessions => (" Sessions ", render_session_lines(app)),
        ViewerKind::Skills => (" Skills ", render_skill_lines(app)),
        ViewerKind::Models => (" Models ", render_model_lines(app)),
        ViewerKind::Themes => (" Themes ", render_theme_lines(app)),
    };

    let t = app.theme;

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            title,
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    let content_width = inner.width.saturating_sub(1);
    let content_height = inner.height as usize;

    let total_lines = Paragraph::new(lines.clone())
        .wrap(Wrap { trim: false })
        .line_count(content_width)
        .max(1);

    let max_scroll = total_lines.saturating_sub(content_height) as u16;
    app.update_viewer_scroll_bounds(max_scroll);
    let scroll = app.viewer_scroll;

    let viewer_inner = Rect {
        width: inner.width.saturating_sub(1),
        ..inner
    };

    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        viewer_inner,
    );

    if total_lines > content_height {
        let mut scrollbar_state = ScrollbarState::default()
            .content_length(total_lines)
            .position(scroll as usize)
            .viewport_content_length(content_height);
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: 1,
            height: inner.height,
        };
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(t.border))
                .thumb_style(Style::default().fg(t.text_muted)),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }
}

/// Draw the footer bar shown below the viewer with navigation hints.
pub fn draw_viewer_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let t = &app.theme;
    let hint = match app.viewer {
        Some(ViewerKind::Agents)
        | Some(ViewerKind::Bundles)
        | Some(ViewerKind::Sessions)
        | Some(ViewerKind::Models)
        | Some(ViewerKind::Themes) => "Up/Down to navigate  Enter to select  Esc to close",
        _ => "Esc to close",
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(" Actions ", Style::default().fg(t.text_muted)));

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {hint}"),
            Style::default().fg(t.text_muted),
        )))
        .block(block),
        area,
    );
}

// ── Line renderers ────────────────────────────────────────────────────────────

fn render_session_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    if app.sessions.is_empty() {
        return vec![Line::from(Span::styled(
            " No sessions found. Use /new to create one.",
            Style::default().fg(t.text_muted),
        ))];
    }

    let mut lines = vec![
        Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(
                "ID",
                Style::default()
                    .fg(t.text_muted)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("          ", Style::default()),
            Span::styled(
                "Agent",
                Style::default()
                    .fg(t.text_muted)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("          ", Style::default()),
            Span::styled(
                "Messages",
                Style::default()
                    .fg(t.text_muted)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("    ", Style::default()),
            Span::styled(
                "Created",
                Style::default()
                    .fg(t.text_muted)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            format!(" ─{}", "─".repeat(70)),
            Style::default().fg(t.border),
        )),
    ];

    for (idx, session) in app.sessions.iter().enumerate() {
        let is_selected = idx == app.selected_session;
        let is_active = app.active_session == Some(session.id);
        let id_str = {
            let s = session.id.to_string();
            s[..8.min(s.len())].to_string()
        };
        let marker = if is_selected && is_active {
            ">* "
        } else if is_selected {
            ">  "
        } else if is_active {
            " * "
        } else {
            "   "
        };
        let (prefix, style) = if is_selected {
            (
                Span::styled(
                    marker,
                    Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
                ),
                Style::default().fg(t.primary),
            )
        } else {
            (
                Span::styled(marker, Style::default().fg(t.text_muted)),
                Style::default().fg(t.text),
            )
        };

        lines.push(Line::from(vec![
            prefix,
            Span::styled(format!("{:<12}", id_str), style),
            Span::styled(format!("{:<15}", session.agent_id), style),
            Span::styled(
                format!("{:<12}", format!("{} msgs", session.message_count)),
                style,
            ),
            Span::styled(session.created_at.format("%Y-%m-%d").to_string(), style),
        ]));
    }
    lines
}

fn render_agent_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    if app.agents.is_empty() {
        return vec![Line::from(Span::styled(
            " No agents found in the current bundle.",
            Style::default().fg(t.text_muted),
        ))];
    }

    app.agents
        .iter()
        .enumerate()
        .map(|(idx, agent_id)| {
            let is_selected = idx == app.selected_agent;
            let is_active = app.active_agent.as_deref() == Some(agent_id.as_str());
            let marker = if is_selected && is_active {
                ">* "
            } else if is_selected {
                ">  "
            } else if is_active {
                " * "
            } else {
                "   "
            };
            let line_style = if is_selected {
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.text)
            };
            let active_style = if is_selected {
                Style::default().fg(t.primary)
            } else {
                Style::default().fg(t.secondary)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(t.text_muted)),
                Span::styled(agent_id.clone(), line_style),
                Span::styled(if is_active { " (active)" } else { "" }, active_style),
            ])
        })
        .collect()
}

fn render_bundle_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    if app.bundles.is_empty() {
        return vec![Line::from(Span::styled(
            " No installed bundles found in ~/.odyssey/bundles.",
            Style::default().fg(t.text_muted),
        ))];
    }

    app.bundles
        .iter()
        .enumerate()
        .map(|(idx, bundle)| {
            let bundle_ref = format!("{}/{}@{}", bundle.namespace, bundle.id, bundle.version);
            let is_selected = idx == app.selected_bundle;
            let is_active = app.bundle_ref == bundle_ref;
            let marker = if is_selected && is_active {
                ">* "
            } else if is_selected {
                ">  "
            } else if is_active {
                " * "
            } else {
                "   "
            };
            let line_style = if is_selected {
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.text)
            };
            let path_style = if is_selected {
                Style::default().fg(t.primary)
            } else {
                Style::default().fg(t.text_muted)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(t.text_muted)),
                Span::styled(format!("{:<28}", bundle_ref), line_style),
                Span::styled(bundle.path.display().to_string(), path_style),
            ])
        })
        .collect()
}

fn render_skill_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    if app.skills.is_empty() {
        return vec![Line::from(Span::styled(
            " No skills configured.",
            Style::default().fg(t.text_muted),
        ))];
    }

    let mut lines = Vec::new();
    for skill in &app.skills {
        let path = skill
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| skill.path.to_string_lossy().to_string());

        lines.push(Line::from(Span::styled(
            format!(" {}", skill.name),
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("   {}", skill.description),
            Style::default().fg(t.text),
        )));
        lines.push(Line::from(Span::styled(
            format!("   {path}"),
            Style::default().fg(t.text_muted),
        )));
        lines.push(Line::from(Span::raw("")));
    }
    lines
}

fn render_model_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    if app.models.is_empty() {
        return vec![Line::from(Span::styled(
            " No models registered.",
            Style::default().fg(t.text_muted),
        ))];
    }

    app.models
        .iter()
        .enumerate()
        .map(|(idx, model_id)| {
            let is_selected = idx == app.selected_model;
            let is_active = model_id == &app.model_id;
            let line_style = if is_selected {
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.text)
            };
            let active_style = if is_selected {
                Style::default().fg(t.primary)
            } else {
                Style::default().fg(t.secondary)
            };
            Line::from(vec![
                Span::styled(
                    format!(" {} ", if is_selected { ">" } else { " " }),
                    line_style,
                ),
                Span::styled(model_id.clone(), line_style),
                Span::styled(if is_active { " (active)" } else { "" }, active_style),
            ])
        })
        .collect()
}

fn render_theme_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut lines = Vec::new();

    // Column header
    lines.push(Line::from(vec![
        Span::styled("   ", Style::default()),
        Span::styled(
            "Name",
            Style::default()
                .fg(t.text_muted)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("            ", Style::default()),
        Span::styled(
            "Preview",
            Style::default()
                .fg(t.text_muted)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!(" ─{}", "─".repeat(50)),
        Style::default().fg(t.border),
    )));

    for (idx, theme) in AVAILABLE_THEMES.iter().enumerate() {
        let is_selected = idx == app.selected_theme;
        let is_active = theme.name == app.theme.name;

        let (prefix, name_style) = if is_selected {
            (
                Span::styled(
                    "  ",
                    Style::default()
                        .fg(theme.primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                Span::styled("   ", Style::default()),
                Style::default().fg(t.text),
            )
        };

        let active_tag = if is_active {
            Span::styled(" (active)", Style::default().fg(t.secondary))
        } else {
            Span::styled("", Style::default())
        };

        // Colour swatches: show each theme's own colors regardless of active theme
        let swatches = vec![
            Span::styled("  ", Style::default().bg(theme.primary)),
            Span::styled("  ", Style::default().bg(theme.secondary)),
            Span::styled("  ", Style::default().bg(theme.accent)),
            Span::styled("  ", Style::default().bg(theme.text)),
            Span::styled("  ", Style::default().bg(Color::Reset)),
        ];

        let mut row = vec![
            prefix,
            Span::styled(format!("{:<14}", theme.name), name_style),
            active_tag,
        ];
        row.extend(swatches);
        lines.push(Line::from(row));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::{
        AVAILABLE_THEMES, draw_viewer, draw_viewer_footer, render_agent_lines, render_bundle_lines,
        render_model_lines, render_session_lines, render_skill_lines, render_theme_lines,
    };
    use crate::app::{App, ViewerKind};
    use chrono::Utc;
    use odyssey_rs_bundle::BundleInstallSummary;
    use odyssey_rs_protocol::{SessionSummary, SkillSummary};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn base_app() -> App {
        let session_id = Uuid::new_v4();
        App {
            bundle_ref: "local/demo@0.1.0".to_string(),
            active_session: Some(session_id),
            active_agent: Some("beta".to_string()),
            model_id: "gpt-4.1".to_string(),
            models: vec!["gpt-4.1-mini".to_string(), "gpt-4.1".to_string()],
            agents: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
            bundles: vec![
                BundleInstallSummary {
                    namespace: "local".to_string(),
                    id: "demo".to_string(),
                    version: "0.1.0".to_string(),
                    path: PathBuf::from("/bundles/demo"),
                },
                BundleInstallSummary {
                    namespace: "team".to_string(),
                    id: "ops".to_string(),
                    version: "1.2.3".to_string(),
                    path: PathBuf::from("/bundles/ops"),
                },
            ],
            sessions: vec![
                SessionSummary {
                    id: Uuid::new_v4(),
                    agent_id: "alpha".to_string(),
                    message_count: 1,
                    created_at: Utc::now(),
                },
                SessionSummary {
                    id: session_id,
                    agent_id: "beta".to_string(),
                    message_count: 3,
                    created_at: Utc::now(),
                },
                SessionSummary {
                    id: Uuid::new_v4(),
                    agent_id: "gamma".to_string(),
                    message_count: 5,
                    created_at: Utc::now(),
                },
            ],
            skills: vec![
                SkillSummary {
                    name: "repo-hygiene".to_string(),
                    description: "Keep repositories clean".to_string(),
                    path: PathBuf::from("/"),
                },
                SkillSummary {
                    name: "review".to_string(),
                    description: "Review code changes".to_string(),
                    path: PathBuf::from("skills/review/SKILL.md"),
                },
            ],
            selected_session: 0,
            selected_agent: 1,
            selected_bundle: 1,
            selected_model: 1,
            selected_theme: 1,
            theme: AVAILABLE_THEMES[1],
            ..App::default()
        }
    }

    fn flatten(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn render_overlay(app: &mut App, height: u16) -> String {
        let backend = TestBackend::new(100, height);
        let mut terminal = Terminal::new(backend).expect("create terminal");
        terminal
            .draw(|frame| draw_viewer(frame, app, frame.area()))
            .expect("draw overlay");
        format!("{}", terminal.backend())
    }

    fn render_footer(app: &App) -> String {
        let backend = TestBackend::new(100, 4);
        let mut terminal = Terminal::new(backend).expect("create terminal");
        terminal
            .draw(|frame| draw_viewer_footer(frame, app, frame.area()))
            .expect("draw footer");
        format!("{}", terminal.backend())
    }

    #[test]
    fn viewer_line_helpers_cover_empty_states() {
        let app = App::default();

        assert_eq!(
            flatten(&render_session_lines(&app)),
            vec![" No sessions found. Use /new to create one.".to_string()]
        );
        assert_eq!(
            flatten(&render_agent_lines(&app)),
            vec![" No agents found in the current bundle.".to_string()]
        );
        assert_eq!(
            flatten(&render_bundle_lines(&app)),
            vec![" No installed bundles found in ~/.odyssey/bundles.".to_string()]
        );
        assert_eq!(
            flatten(&render_skill_lines(&app)),
            vec![" No skills configured.".to_string()]
        );
        assert_eq!(
            flatten(&render_model_lines(&app)),
            vec![" No models registered.".to_string()]
        );
    }

    #[test]
    fn viewer_line_helpers_cover_selected_active_and_preview_states() {
        let app = base_app();

        assert!(
            flatten(&render_session_lines(&app))
                .iter()
                .any(|line| line.contains(">  ") && line.contains("alpha"))
        );
        assert!(
            flatten(&render_session_lines(&app))
                .iter()
                .any(|line| line.contains(" * ") && line.contains("beta"))
        );
        assert!(
            flatten(&render_agent_lines(&app))
                .iter()
                .any(|line| line.contains("beta") && line.contains("(active)"))
        );
        assert!(
            flatten(&render_bundle_lines(&app))
                .iter()
                .any(|line| line.contains("team/ops@1.2.3") && line.contains("/bundles/ops"))
        );
        assert!(
            flatten(&render_skill_lines(&app))
                .iter()
                .any(|line| line.trim() == "/")
        );
        assert!(
            flatten(&render_model_lines(&app))
                .iter()
                .any(|line| line.contains("gpt-4.1") && line.contains("(active)"))
        );
        assert!(
            flatten(&render_theme_lines(&app))
                .iter()
                .any(|line| line.contains(AVAILABLE_THEMES[1].name) && line.contains("(active)"))
        );
    }

    #[test]
    fn draw_viewer_is_noop_when_closed_and_footer_uses_contextual_hints() {
        let mut app = App {
            viewer_scroll: 7,
            viewer_max_scroll: 11,
            ..App::default()
        };

        let rendered = render_overlay(&mut app, 8);
        let blank_line = format!("\"{}\"", " ".repeat(100));
        assert_eq!(rendered.lines().count(), 8);
        assert!(rendered.lines().all(|line| line == blank_line));
        assert_eq!(app.viewer_scroll, 7);
        assert_eq!(app.viewer_max_scroll, 11);

        app.viewer = Some(ViewerKind::Skills);
        assert!(render_footer(&app).contains("Esc to close"));
        app.viewer = Some(ViewerKind::Themes);
        assert!(render_footer(&app).contains("Up/Down to navigate"));
    }

    #[test]
    fn draw_viewer_updates_scroll_bounds_and_renders_each_overlay_variant() {
        let mut app = base_app();
        app.skills = (0..8)
            .map(|idx| SkillSummary {
                name: format!("skill-{idx}"),
                description: "Scrollable skill".to_string(),
                path: PathBuf::from(format!("skills/{idx}/SKILL.md")),
            })
            .collect();

        for kind in [
            ViewerKind::Sessions,
            ViewerKind::Agents,
            ViewerKind::Bundles,
            ViewerKind::Models,
            ViewerKind::Themes,
            ViewerKind::Skills,
        ] {
            app.viewer = Some(kind);
            let rendered = render_overlay(&mut app, 8);
            assert!(!rendered.trim().is_empty());
        }

        assert!(app.viewer_max_scroll > 0);
    }
}
