use std::path::Path;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap},
};

use super::WorkerStatus as UiWorkerStatus;
use super::bottom_bar::{ActivePane, RightPaneMode};
use crate::cookbook::{PatchStatus, Recipe};
use crate::prepare::{self, PrepState};
use crate::theme::*;

pub fn truncate_end(s: &str, max_len: usize) -> String {
    let count = s.chars().count();
    if count <= max_len {
        return s.to_string();
    }
    if max_len <= 1 {
        return s.chars().take(max_len).collect();
    }
    let keep = max_len.saturating_sub(1);
    let s_chars: Vec<char> = s.chars().collect();
    let front: String = s_chars[..keep].iter().collect();
    format!("{front}…")
}

pub fn clean_artifact(name: &str, max_len: usize) -> String {
    let clean = name.strip_suffix(".pkg.tar.zst").unwrap_or(name);
    truncate_end(clean, max_len)
}

pub fn draw_panes(
    f: &mut Frame,
    left_area: Rect,
    right_area: Rect,
    recipes: &[Recipe],
    cache_dir: &Path,
    recipe_cursor: usize,
    patch_cursor: usize,
    active_pane: ActivePane,
    right_mode: RightPaneMode,
    worker_status: &[Option<UiWorkerStatus>],
    cook_status: &[Option<String>],
    artifacts: &[Option<String>],
    log_buffers: &[Vec<String>],
    log_scroll: usize,
    auto_scroll: bool,
) {
    draw_left_pane(
        f,
        left_area,
        recipes,
        cache_dir,
        recipe_cursor,
        active_pane == ActivePane::Left,
        worker_status,
        cook_status,
        artifacts,
    );

    draw_right_pane(
        f,
        right_area,
        recipes,
        recipe_cursor,
        patch_cursor,
        active_pane == ActivePane::Right,
        right_mode,
        log_buffers,
        log_scroll,
        auto_scroll,
    );
}

fn draw_left_pane(
    f: &mut Frame,
    area: Rect,
    recipes: &[Recipe],
    cache_dir: &Path,
    recipe_cursor: usize,
    is_active: bool,
    worker_status: &[Option<UiWorkerStatus>],
    cook_status: &[Option<String>],
    artifacts: &[Option<String>],
) {
    let border_style = if is_active {
        Style::default().fg(NC_WHITE)
    } else {
        Style::default().fg(NC_CYAN)
    };

    let title_line = Line::from(vec![Span::styled(
        " [ Recipes & Artifacts ] ",
        Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD),
    )]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(border_style)
        .style(Style::default().bg(NC_BLUE).fg(NC_WHITE))
        .title(title_line);

    let total_count = recipes.len();
    let visible_rows = area.height.saturating_sub(3) as usize;

    let offset = if recipe_cursor >= visible_rows {
        recipe_cursor - visible_rows + 1
    } else {
        0
    };

    let status_w = 11;
    let artifact_w = 26.min(area.width.saturating_sub(28) as usize);
    let name_w = (area
        .width
        .saturating_sub(status_w as u16 + artifact_w as u16 + 8) as usize)
        .max(8);

    let header = format!(
        "   {:<name_w$} {:<status_w$} {:<artifact_w$}",
        "Recipe",
        "Status",
        "Artifact",
        name_w = name_w,
        status_w = status_w,
        artifact_w = artifact_w,
    );

    let items: Vec<ListItem> = recipes
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_rows)
        .map(|(idx, recipe)| {
            let is_prepared = prepare::state(cache_dir, &recipe.name) == PrepState::Ready;

            let status = match worker_status[idx] {
                Some(UiWorkerStatus::Preparing) => "[prep]".to_owned(),
                Some(UiWorkerStatus::Resetting) => "[reset]".to_owned(),
                Some(UiWorkerStatus::Cooking) => "[cooking]".to_owned(),
                Some(UiWorkerStatus::Installing) => "[install]".to_owned(),
                None if cook_status[idx].as_deref() == Some("[update]") => "[update]".to_owned(),
                None if cook_status[idx].as_deref() == Some("[installed]") => {
                    "[installed]".to_owned()
                }
                None if cook_status[idx]
                    .as_deref()
                    .is_some_and(|s| s.contains("install failed")) =>
                {
                    "[failed]".to_owned()
                }
                None if cook_status[idx].as_deref() == Some("[cooked successfully]") => {
                    "[cooked]".to_owned()
                }
                None if artifacts[idx].is_some() => "[cooked]".to_owned(),
                None if cook_status[idx]
                    .as_deref()
                    .is_some_and(|s| s.contains("failed")) =>
                {
                    "[failed]".to_owned()
                }
                None if is_prepared => "[ready]".to_owned(),
                None => "[unprep]".to_owned(),
            };

            let artifact_info = if let Some(ref art) = artifacts[idx] {
                clean_artifact(art, artifact_w)
            } else {
                format!("({} patches)", recipe.patches.len())
            };

            let trunc_name = truncate_end(&recipe.name, name_w);
            let trunc_status = truncate_end(&status, status_w);
            let trunc_art = truncate_end(&artifact_info, artifact_w);

            let row_text = format!(
                "   {:<name_w$} {:<status_w$} {:<artifact_w$}",
                trunc_name,
                trunc_status,
                trunc_art,
                name_w = name_w,
                status_w = status_w,
                artifact_w = artifact_w,
            );

            if idx == recipe_cursor && is_active {
                ListItem::new(row_text).style(
                    Style::default()
                        .bg(NC_CYAN)
                        .fg(NC_BLACK)
                        .add_modifier(Modifier::BOLD),
                )
            } else if idx == recipe_cursor && !is_active {
                ListItem::new(row_text)
                    .style(Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD))
            } else if artifacts[idx].is_some() {
                ListItem::new(row_text).style(Style::default().fg(NC_WHITE))
            } else {
                ListItem::new(row_text).style(Style::default().fg(NC_GRAY))
            }
        })
        .collect();

    f.render_widget(block, area);

    if area.height > 3 && area.width > 20 {
        let header_rect = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: 1,
        };
        f.render_widget(
            Paragraph::new(header).style(
                Style::default()
                    .fg(NC_CYAN)
                    .bg(NC_BLUE)
                    .add_modifier(Modifier::BOLD),
            ),
            header_rect,
        );

        let list_rect = Rect {
            x: area.x + 1,
            y: area.y + 2,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(3),
        };
        f.render_widget(List::new(items), list_rect);
    }

    let status_str = format!(
        " {}/{} recipes ",
        if total_count == 0 {
            0
        } else {
            recipe_cursor + 1
        },
        total_count
    );
    let status_rect = Rect {
        x: area.x + area.width.saturating_sub(status_str.len() as u16 + 2),
        y: area.y + area.height - 1,
        width: status_str.len() as u16,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(status_str).style(Style::default().fg(NC_CYAN).bg(NC_BLUE)),
        status_rect,
    );
}

fn draw_right_pane(
    f: &mut Frame,
    area: Rect,
    recipes: &[Recipe],
    recipe_cursor: usize,
    patch_cursor: usize,
    is_active: bool,
    right_mode: RightPaneMode,
    log_buffers: &[Vec<String>],
    log_scroll: usize,
    auto_scroll: bool,
) {
    let border_style = if is_active {
        Style::default().fg(NC_WHITE)
    } else {
        Style::default().fg(NC_CYAN)
    };

    let recipe = recipes.get(recipe_cursor);
    let recipe_name = recipe.map(|r| r.name.as_str()).unwrap_or("No Recipe");

    match right_mode {
        RightPaneMode::Patches => {
            let title_line = Line::from(vec![Span::styled(
                format!(" [ Patches: {} ] ", recipe_name),
                Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD),
            )]);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(border_style)
                .style(Style::default().bg(NC_BLUE).fg(NC_WHITE))
                .title(title_line);

            let patches = recipe.map(|r| r.patches.as_slice()).unwrap_or(&[]);
            let total_count = patches.len();
            let selected_count = patches.iter().filter(|p| p.selected).count();

            let visible_rows = area.height.saturating_sub(3) as usize;

            let offset = if patch_cursor >= visible_rows {
                patch_cursor - visible_rows + 1
            } else {
                0
            };

            let status_w = 8;
            let use_w = 5;
            let file_w = (area
                .width
                .saturating_sub(status_w as u16 + use_w as u16 + 8)
                as usize)
                .max(10);

            let header = format!(
                "   {:<file_w$} {:<status_w$} {:>use_w$}",
                "Patch File",
                "Status",
                "Use",
                file_w = file_w,
                status_w = status_w,
                use_w = use_w,
            );

            let items: Vec<ListItem> = patches
                .iter()
                .enumerate()
                .skip(offset)
                .take(visible_rows)
                .map(|(idx, patch)| {
                    let use_mark = if patch.selected { "[✔]" } else { "[ ]" };

                    let status_str = match patch.status {
                        PatchStatus::Clean => " ✔ OK   ",
                        PatchStatus::Failed => " ✗ Conf ",
                        PatchStatus::Checking => " ⟳ Chk  ",
                        PatchStatus::Unknown => " ? Unk  ",
                    };

                    let trunc_file = truncate_end(patch.file_name(), file_w);

                    let row_text = format!(
                        "   {:<file_w$} {:<status_w$} {:>use_w$}",
                        trunc_file,
                        status_str,
                        use_mark,
                        file_w = file_w,
                        status_w = status_w,
                        use_w = use_w,
                    );

                    if idx == patch_cursor && is_active {
                        ListItem::new(row_text).style(
                            Style::default()
                                .bg(NC_CYAN)
                                .fg(NC_BLACK)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else if idx == patch_cursor && !is_active {
                        ListItem::new(row_text)
                            .style(Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD))
                    } else if patch.selected {
                        ListItem::new(row_text)
                            .style(Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD))
                    } else {
                        ListItem::new(row_text).style(Style::default().fg(NC_WHITE))
                    }
                })
                .collect();

            f.render_widget(block, area);

            if area.height > 3 && area.width > 20 {
                let header_rect = Rect {
                    x: area.x + 1,
                    y: area.y + 1,
                    width: area.width.saturating_sub(2),
                    height: 1,
                };
                f.render_widget(
                    Paragraph::new(header).style(
                        Style::default()
                            .fg(NC_CYAN)
                            .bg(NC_BLUE)
                            .add_modifier(Modifier::BOLD),
                    ),
                    header_rect,
                );

                let list_rect = Rect {
                    x: area.x + 1,
                    y: area.y + 2,
                    width: area.width.saturating_sub(2),
                    height: area.height.saturating_sub(3),
                };
                f.render_widget(List::new(items), list_rect);
            }

            let status_str = format!(
                " {}/{} patches ({} sel) ",
                if total_count == 0 {
                    0
                } else {
                    patch_cursor + 1
                },
                total_count,
                selected_count
            );
            let status_rect = Rect {
                x: area.x + area.width.saturating_sub(status_str.len() as u16 + 2),
                y: area.y + area.height - 1,
                width: status_str.len() as u16,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(status_str).style(Style::default().fg(NC_CYAN).bg(NC_BLUE)),
                status_rect,
            );
        }

        RightPaneMode::BuildLog => {
            let title_line = Line::from(vec![Span::styled(
                format!(" [ Build Log: {} (F3/Esc: Patches) ] ", recipe_name),
                Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD),
            )]);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(border_style)
                .style(Style::default().bg(NC_BLUE).fg(NC_WHITE))
                .title(title_line);

            let lines = log_buffers
                .get(recipe_cursor)
                .map(|b| b.as_slice())
                .unwrap_or(&[]);
            let total_lines = lines.len();
            let visible_rows = area.height.saturating_sub(2) as usize;

            let offset = if auto_scroll {
                total_lines.saturating_sub(visible_rows)
            } else {
                log_scroll.min(total_lines.saturating_sub(visible_rows))
            };

            let log_text: Vec<Line> = lines
                .iter()
                .skip(offset)
                .take(visible_rows)
                .map(|line| Line::from(Span::raw(line)))
                .collect();

            let paragraph = Paragraph::new(log_text)
                .block(block)
                .wrap(Wrap { trim: false });

            f.render_widget(paragraph, area);

            let auto_str = if auto_scroll { "[AUTO]" } else { "" };
            let status_str = format!(
                " {}/{} lines {} ",
                if total_lines == 0 {
                    0
                } else {
                    (offset + visible_rows).min(total_lines)
                },
                total_lines,
                auto_str
            );
            let status_rect = Rect {
                x: area.x + area.width.saturating_sub(status_str.len() as u16 + 2),
                y: area.y + area.height - 1,
                width: status_str.len() as u16,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(status_str).style(Style::default().fg(NC_YELLOW).bg(NC_BLUE)),
                status_rect,
            );
        }
    }
}
