use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::theme::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RightPaneMode {
    Patches,
    BuildLog,
}

pub fn draw_bottom_bar(
    f: &mut Frame,
    area: Rect,
    active_pane: ActivePane,
    right_mode: RightPaneMode,
    enter_label: &str,
) {
    let f_keys = match (active_pane, right_mode) {
        (ActivePane::Left, _) => vec![
            ("F1", "Help"),
            ("Enter", enter_label),
            ("F2", "Cook"),
            ("F3", "Log"),
            ("F4", "Reset"),
            ("F5", "SyncDB"),
            ("F8", "Abort"),
            ("F10", "Quit"),
        ],
        (ActivePane::Right, RightPaneMode::Patches) => vec![
            ("F1", "Help"),
            ("Space", "Select"),
            ("Enter", "Verify"),
            ("F2", "VerAll"),
            ("F3", "Log"),
            ("F4", "Reset"),
            ("F8", "Abort"),
            ("F10", "Quit"),
        ],
        (ActivePane::Right, RightPaneMode::BuildLog) => vec![
            ("F1", "Help"),
            ("Space", "AutoScroll"),
            ("F3", "Patches"),
            ("F8", "Abort"),
            ("F10", "Quit"),
        ],
    };

    let mut spans = Vec::new();
    for (key, label) in f_keys {
        spans.push(Span::styled(
            format!(" {} ", key),
            Style::default()
                .fg(NC_BLACK)
                .bg(NC_CYAN)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{} ", label),
            Style::default().fg(NC_BLACK).bg(NC_GRAY),
        ));
        spans.push(Span::raw(" "));
    }

    let p = Paragraph::new(Line::from(spans)).style(Style::default().bg(NC_GRAY));
    f.render_widget(p, area);
}
