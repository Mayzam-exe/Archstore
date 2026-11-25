//! Rendering — the Play-Store-style shell: header band, search field, results
//! list with a details pane, footer, and the modal overlays.
//!
//! Colour is the whole point of this screen, so the palette is declared once
//! here and every surface (band, field, chips, stripes) is filled rather than
//! left to the terminal default.

use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::data::{dwidth, pad, truncate, Pkg, Src};
use crate::{App, ConfirmOp, LoadState, TABS, ITEM_H};

// ------------------------------------------------------------------ palette

const ACCENT: Color = Color::Rgb(0x37, 0xf4, 0x99); // their green
const AURY: Color = Color::Rgb(0xe9, 0xf9, 0x41); // aur yellow
const PURPLE: Color = Color::Rgb(0x90, 0x71, 0xf4); // repo violet
const ORANGE: Color = Color::Rgb(0xf0, 0x8d, 0x49); // core orange
const CYAN: Color = Color::Rgb(0x61, 0xaf, 0xef); // multilib / details
const DANGER: Color = Color::Rgb(0xf9, 0x51, 0x5d);
const FAINT: Color = Color::Rgb(0x74, 0x77, 0x8c); // tertiary text
const DIM: Color = Color::Rgb(0x9a, 0x9e, 0xb8); // secondary text
const GREYCHIP: Color = Color::Rgb(0x8b, 0x93, 0xa7);

const SEL: Color = Color::Rgb(0x35, 0x39, 0x5e); // selected row
const STRIPE: Color = Color::Rgb(0x21, 0x22, 0x34); // alternating row
const PANEL: Color = Color::Rgb(0x1a, 0x1b, 0x26); // panel fill
const HEADER: Color = Color::Rgb(0x26, 0x21, 0x40); // violet top band
const FIELD: Color = Color::Rgb(0x14, 0x15, 0x1d); // search input fill
const EDGE: Color = Color::Rgb(0x4b, 0x46, 0x74); // idle violet border
const EDGE2: Color = Color::Rgb(0x2f, 0x5f, 0x7a); // idle cyan border
const RULE: Color = Color::Rgb(0x3a, 0x3c, 0x52); // separators
const INK: Color = Color::Black;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The search field always shows this marker; the text cursor sits after it.
/// Plain `/` on purpose: it renders in every font and doubles as a reminder of
/// the key that focuses the field.
pub const SEARCH_PREFIX: &str = "/ ";
pub const SEARCH_PREFIX_W: u16 = 2;

// --------------------------------------------------------------------- draw

pub fn draw(app: &mut App, f: &mut Frame) {
    let area = f.area();
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(app, f, rows[0]);
    draw_search(app, f, rows[1]);
    draw_main(app, f, rows[2]);
    draw_footer(app, f, rows[3]);

    if app.help {
        draw_help(f, area);
    }
    if app.detail_open {
        draw_detail(app, f, area);
    }
    if app.confirm.is_some() {
        draw_confirm(app, f, area);
    }

    // text cursor in the search field while it is focused
    if app.focus_search && !app.help && !app.detail_open && app.confirm.is_none() {
        let r = app.rects.search;
        if r.width > 2 + SEARCH_PREFIX_W {
            let chars: Vec<char> = app.query.chars().collect();
            let pre: String = chars[..app.cursor.min(chars.len())].iter().collect();
            f.set_cursor_position(Position {
                x: r.x + 1 + SEARCH_PREFIX_W + dwidth(&pre) as u16,
                y: r.y + 1,
            });
        }
    }
}

// ------------------------------------------------------------------ header

fn header_status(app: &App) -> (String, Style) {
    let busy = app.search_loading
        || app.top_state == LoadState::Loading
        || app.updated_state == LoadState::Loading
        || app.installed_state == LoadState::Loading;
    if busy {
        return (
            format!("{} loading ", SPINNER[app.spinner % SPINNER.len()]),
            Style::default().fg(AURY).add_modifier(Modifier::BOLD),
        );
    }
    if app.net_down {
        return (
            "⚠ offline results ".to_string(),
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        );
    }
    let n = app.active_list().len();
    if app.tab == 3 && !app.query.trim().is_empty() {
        // the Installed tab doubles as a filter — say so instead of "0 installed"
        return (
            format!("{n} of {} match ", app.installed_all.len()),
            Style::default().fg(AURY).add_modifier(Modifier::BOLD),
        );
    }
    let word = match app.tab {
        0 if app.query.trim().is_empty() => "popular",
        0 => "results",
        1 => "popular",
        2 => "updated",
        _ => "installed",
    };
    (
        format!("{n} {word} "),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    )
}

fn draw_header(app: &mut App, f: &mut Frame, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    let logo = " ▶ ARCHSTORE ";
    spans.push(Span::styled(
        logo,
        Style::default()
            .fg(Color::White)
            .bg(PURPLE)
            .add_modifier(Modifier::BOLD),
    ));
    let mut x = area.x + dwidth(logo) as u16;
    let mut tabs = vec![Rect::default(); TABS.len()];
    for (i, t) in TABS.iter().enumerate() {
        let label = format!(" {} ", t);
        let w = dwidth(&label) as u16;
        tabs[i] = Rect { x, y: area.y, width: w, height: 1 };
        x += w;
        let st = if i == app.tab {
            Style::default()
                .fg(INK)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        spans.push(Span::styled(label, st));
    }
    app.rects.tabs = tabs;

    let (status, sstyle) = header_status(app);
    let used: usize = spans.iter().map(|s| dwidth(&s.content)).sum();
    let gap = (area.width as usize)
        .saturating_sub(used + dwidth(&status))
        .max(1);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled(status, sstyle));

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(HEADER)),
        area,
    );
}

// ----------------------------------------------------------------- search

fn draw_search(app: &mut App, f: &mut Frame, area: Rect) {
    app.rects.search = area;
    let focused = app.focus_search;
    let (border, title_style) = if focused {
        (ACCENT, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
    } else {
        (EDGE, Style::default().fg(DIM))
    };
    let title = if focused { " search " } else { " / search " };
    let block = Block::bordered()
        .title(Span::styled(title, title_style))
        .border_style(Style::default().fg(border));

    let prefix_style = if focused {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(PURPLE)
    };

    let inner_w = (area.width.saturating_sub(2 + SEARCH_PREFIX_W)) as usize;
    let body = if app.query.is_empty() {
        if focused {
            Line::from("")
        } else {
            let ph = match app.tab {
                1 | 2 => "type to search packages…",
                3 => "filter installed packages…",
                _ => "type a name or description — try “video editor”",
            };
            Line::from(Span::styled(truncate(ph, inner_w), Style::default().fg(FAINT)))
        }
    } else {
        let chars: Vec<char> = app.query.chars().collect();
        let ci = app.cursor.min(chars.len());
        Line::from(vec![
            Span::raw(chars[..ci].iter().collect::<String>()),
            Span::raw(chars[ci..].iter().collect::<String>()),
        ])
    };

    let mut spans = vec![Span::styled(SEARCH_PREFIX, prefix_style)];
    spans.extend(body.spans);
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .block(block)
            .style(Style::default().bg(FIELD)),
        area,
    );
}

// ------------------------------------------------------------------- main

fn draw_main(app: &mut App, f: &mut Frame, area: Rect) {
    let (list_area, side) = if area.width >= 100 {
        let h = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);
        (h[0], Some(h[1]))
    } else {
        (area, None)
    };

    let (title, tcolor) = match app.tab {
        0 if app.query.trim().is_empty() => (" popular right now ", AURY),
        0 => (" results ", ACCENT),
        1 => (" most popular aur ", AURY),
        2 => (" recently updated ", CYAN),
        3 if !app.query.trim().is_empty() => (" installed · filtered ", AURY),
        _ => (" installed ", ACCENT),
    };

    // keep the selected row inside the window (mirrors what List will render)
    let inner_h = list_area.height.saturating_sub(2) as usize;
    let visible = (inner_h / ITEM_H).max(1);
    let len = app.active_list().len();
    sync_offset(app, visible, len);

    let block = Block::bordered()
        .title(Span::styled(
            title,
            Style::default().fg(tcolor).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(EDGE))
        .style(Style::default().bg(PANEL));
    let inner = block.inner(list_area);
    app.rects.list = inner;

    let items = build_items(app, inner.width);
    let selected = if items.is_empty() {
        None
    } else {
        Some(app.selected.min(items.len() - 1))
    };
    let mut state = ListState::default()
        .with_selected(selected)
        .with_offset(app.offset);
    let list = List::new(items)
        .block(block)
        .highlight_symbol(Span::styled(
            "▶ ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .highlight_style(Style::default()); // row colour is baked in per item
    f.render_stateful_widget(list, list_area, &mut state);

    if len > visible {
        let mut sb =
            ScrollbarState::new(len).position(app.selected.min(len.saturating_sub(1)));
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(ACCENT))
            .track_style(Style::default().fg(Color::Rgb(0x2a, 0x2c, 0x3d)));
        f.render_stateful_widget(bar, list_area, &mut sb);
    }

    if let Some(a) = side {
        draw_side(app, f, a);
    }
}

pub(crate) fn sync_offset(app: &mut App, visible: usize, len: usize) {
    if len == 0 {
        app.offset = 0;
        return;
    }
    let sel = app.selected.min(len - 1);
    if sel < app.offset {
        app.offset = sel;
    } else if visible > 0 && sel >= app.offset + visible {
        app.offset = sel + 1 - visible;
    }
    if app.offset >= len {
        app.offset = len.saturating_sub(1);
    }
}

// --------------------------------------------------------------- list rows

/// A filled pill, the way a store labels things.
fn chip(text: &str, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::default()
            .fg(INK)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    )
}

/// Source badge: colour carries the repository, the label names it.
fn repo_badge(p: &Pkg) -> (String, Color) {
    if p.src == Src::Aur {
        return ("aur".to_string(), AURY);
    }
    let c = match p.repo.as_str() {
        "core" => ORANGE,
        "extra" => PURPLE,
        "multilib" => CYAN,
        "testing" | "core-testing" | "extra-testing" => DANGER,
        _ => GREYCHIP,
    };
    (p.repo.clone(), c)
}

fn build_items(app: &App, width: u16) -> Vec<ListItem<'static>> {
    let pkgs = app.active_list();
    // the highlight symbol occupies two columns on every row
    let eff = (width as usize).saturating_sub(2);
    let vw = pkgs
        .iter()
        .map(|p| dwidth(&p.version))
        .max()
        .unwrap_or(6)
        .clamp(6, 18);

    pkgs.iter()
        .enumerate()
        .map(|(i, p)| {
            let sel = i == app.selected;
            let (label, c) = repo_badge(p);

            let name_style = if sel {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            let mut l1 = vec![
                chip(&label, c),
                Span::raw(" "),
                Span::styled(p.name.clone(), name_style),
                Span::raw(" "),
                Span::styled(
                    pad(&truncate(&p.version, vw), vw),
                    Style::default().fg(if sel { DIM } else { FAINT }),
                ),
            ];
            if p.src == Src::Aur {
                l1.push(Span::raw(" "));
                l1.push(Span::styled(
                    format!("▲{}", p.votes),
                    Style::default().fg(AURY).add_modifier(Modifier::BOLD),
                ));
            }

            // line 2 leads with the state chips so they never get truncated away
            let mut used = 0usize;
            let mut l2: Vec<Span> = Vec::new();
            if p.installed || app.installed.contains(&p.name) {
                let c = chip("installed", ACCENT);
                used += dwidth(&c.content);
                l2.push(c);
                l2.push(Span::raw(" "));
                used += 1;
            }
            if p.outdated {
                let c = chip("out of date", DANGER);
                used += dwidth(&c.content);
                l2.push(c);
                l2.push(Span::raw(" "));
                used += 1;
            }
            let avail = eff.saturating_sub(used + 4);
            if !p.desc.is_empty() {
                l2.push(Span::styled(
                    truncate(&p.desc, avail),
                    Style::default().fg(FAINT),
                ));
            }

            let row_bg = if sel {
                Style::default().bg(SEL)
            } else if i % 2 == 1 {
                Style::default().bg(STRIPE)
            } else {
                Style::default()
            };
            ListItem::new(vec![Line::from(l1), Line::from(l2)]).style(row_bg)
        })
        .collect()
}

// ------------------------------------------------------------------ side

fn draw_side(app: &mut App, f: &mut Frame, area: Rect) {
    let block = Block::bordered()
        .title(Span::styled(
            " details ",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(EDGE2))
        .style(Style::default().bg(PANEL));
    app.rects.side = block.inner(area);

    let pkg = app.active_list().get(app.selected).cloned();
    let text = match pkg {
        None => Text::from(vec![Line::from(Span::styled(
            "  select a package to see details",
            Style::default().fg(FAINT),
        ))]),
        Some(p) => {
            let mut lines: Vec<Line> = vec![];
            lines.push(Line::from(Span::styled(
                p.name.clone(),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
            let (label, c) = repo_badge(&p);
            let mut head = vec![chip(&label, c), Span::raw("  ")];
            head.push(Span::styled(
                p.version.clone(),
                Style::default().fg(DIM),
            ));
            if p.src == Src::Aur {
                head.push(Span::raw("  "));
                head.push(Span::styled(
                    format!("▲{} ({:.2})", p.votes, p.popularity),
                    Style::default().fg(AURY).add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(head));
            lines.push(Line::from(Span::styled(
                "─".repeat(24),
                Style::default().fg(RULE),
            )));
            if !p.desc.is_empty() {
                lines.push(Line::from(Span::styled(
                    p.desc.clone(),
                    Style::default().fg(DIM),
                )));
                lines.push(Line::from(""));
            }
            if !p.maintainer.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("maintainer  ", Style::default().fg(FAINT)),
                    Span::styled(p.maintainer.clone(), Style::default().fg(CYAN)),
                ]));
            }
            if p.installed || app.installed.contains(&p.name) {
                lines.push(Line::from(vec![
                    Span::styled("✓ ", Style::default().fg(ACCENT)),
                    Span::styled(
                        "installed locally",
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            if p.outdated {
                lines.push(Line::from(vec![
                    Span::styled("✗ ", Style::default().fg(DANGER)),
                    Span::styled(
                        "out of date",
                        Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Enter — full details",
                Style::default().fg(FAINT),
            )));
            lines.push(Line::from(Span::styled(
                "i — install",
                Style::default().fg(ACCENT),
            )));
            if app.is_installed(&p) {
                lines.push(Line::from(Span::styled(
                    "d — remove",
                    Style::default().fg(DANGER),
                )));
            }
            Text::from(lines)
        }
    };
    f.render_widget(
        Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: true }),
        area,
    );
}

// ----------------------------------------------------------------- footer

fn key(k: &str) -> Span<'static> {
    Span::styled(
        k.to_string(),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )
}
fn lbl(t: &str) -> Span<'static> {
    Span::styled(format!(" {t}"), Style::default().fg(DIM))
}
fn sep() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(RULE))
}

fn draw_footer(app: &App, f: &mut Frame, area: Rect) {
    let hints: Vec<Span> = if app.confirm.is_some() {
        vec![key("y"), lbl("yes"), sep(), key("n"), lbl("cancel")]
    } else if app.detail_open {
        let mut v = vec![key("↑↓"), lbl("scroll"), sep(), key("i"), lbl("install")];
        if app.detail_can_remove() {
            v.extend([sep(), key("d"), lbl("remove")]);
        }
        v.extend([sep(), key("Esc"), lbl("back")]);
        v
    } else if app.help {
        vec![key("Esc"), lbl("close")]
    } else if app.focus_search {
        vec![
            key("type"),
            lbl("to search"),
            sep(),
            key("↑↓"),
            lbl("pick"),
            sep(),
            key("Enter"),
            lbl("done"),
            sep(),
            key("Esc"),
            lbl("back"),
            sep(),
            key("Ctrl+C"),
            lbl("quit"),
        ]
    } else {
        let mut v = vec![
            key("↑↓"),
            lbl("move"),
            sep(),
            key("Enter"),
            lbl("open"),
            sep(),
            key("i"),
            lbl("install"),
        ];
        if app.can_remove() {
            v.extend([sep(), key("d"), lbl("remove")]);
        }
        v.extend([
            sep(),
            key("/"),
            lbl("search"),
            sep(),
            key("Tab"),
            lbl("tabs"),
            sep(),
            key("q"),
            lbl("quit"),
        ]);
        v
    };

    let (right, rstyle) = if let Some((m, ok, _)) = &app.toast {
        (
            m.clone(),
            Style::default()
                .fg(if *ok { ACCENT } else { DANGER })
                .add_modifier(Modifier::BOLD),
        )
    } else if app.search_loading {
        (
            format!("{} searching", SPINNER[app.spinner % SPINNER.len()]),
            Style::default().fg(AURY).add_modifier(Modifier::BOLD),
        )
    } else {
        (app.status.clone(), Style::default().fg(CYAN))
    };

    let rw = dwidth(&right) as u16 + 1;
    let h = Layout::horizontal([Constraint::Min(0), Constraint::Length(rw)]).split(area);
    let bg = Style::default().bg(PANEL);
    f.render_widget(Paragraph::new(Line::from(hints)).style(bg), h[0]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(right, rstyle)))
            .alignment(Alignment::Right)
            .style(bg),
        h[1],
    );
}

// ---------------------------------------------------------------- overlays

fn center(area: Rect, pw: u16, ph: u16) -> Rect {
    let w = ((area.width as u32 * pw as u32) / 100)
        .max(10)
        .min(u32::from(area.width)) as u16;
    let h = ((area.height as u32 * ph as u32) / 100)
        .max(5)
        .min(u32::from(area.height)) as u16;
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_help(f: &mut Frame, area: Rect) {
    let a = center(area, 74, 76);
    f.render_widget(Clear, a);
    let block = Block::bordered()
        .title(Span::styled(
            " keys ",
            Style::default().fg(INK).bg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(PANEL));

    let rows: &[(&str, &str)] = &[
        ("type", "search the repos + AUR as you type"),
        ("↑ ↓  PgUp PgDn", "move through the results"),
        ("Enter", "open the package page"),
        ("i", "install (asks first)"),
        ("d", "remove an installed package (asks first)"),
        ("/", "focus the search bar"),
        ("Tab  /  1-4", "switch tab"),
        ("r", "refresh this tab"),
        ("F1  or  ?", "this help"),
        ("Esc", "leave the search bar / close overlays"),
        ("q  /  Ctrl+C", "quit — Ctrl+C works from anywhere"),
    ];
    let mut lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(
                    pad(&format!("  {k}"), 21),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(*d, Style::default().fg(DIM)),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  no modes, no commands to learn — it just works.",
        Style::default().fg(AURY),
    )));
    f.render_widget(Paragraph::new(lines).block(block), a);
}

fn draw_detail(app: &App, f: &mut Frame, area: Rect) {
    let a = center(area, 86, 84);
    f.render_widget(Clear, a);
    let block = Block::bordered()
        .title(Span::styled(
            format!(" {} ", app.detail_target),
            Style::default().fg(INK).bg(CYAN).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(CYAN))
        .style(Style::default().bg(PANEL));
    let body = if app.detail_loading {
        Text::from(vec![Line::from(Span::styled(
            format!(
                "  {} loading details…",
                SPINNER[app.spinner % SPINNER.len()]
            ),
            Style::default().fg(AURY),
        ))])
    } else if let Some(d) = &app.detail {
        let mut lines: Vec<Line> = vec![];
        let (label, c) = if d.src == Src::Aur {
            ("aur", AURY)
        } else {
            ("repo", PURPLE)
        };
        lines.push(Line::from(vec![
            chip(label, c),
            Span::raw("  "),
            Span::styled(
                d.name.clone(),
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));
        for (k, v) in &d.rows {
            if v.is_empty() || v == "None" {
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(pad(k, 18), Style::default().fg(CYAN)),
                Span::styled(v.clone(), Style::default().fg(DIM)),
            ]));
        }
        lines.push(Line::from(""));
        let mut hint = vec![key("i"), lbl("install")];
        if app.detail_can_remove() {
            hint.extend([sep(), key("d"), lbl("remove")]);
        }
        hint.extend([sep(), key("Esc"), lbl("back")]);
        lines.push(Line::from(hint));
        Text::from(lines)
    } else {
        Text::from(vec![Line::from(Span::styled(
            "  could not fetch details for this package",
            Style::default().fg(DANGER),
        ))])
    };
    f.render_widget(
        Paragraph::new(body)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        a,
    );
}

fn draw_confirm(app: &App, f: &mut Frame, area: Rect) {
    let Some((p, op)) = &app.confirm else { return };
    let removing = *op == ConfirmOp::Remove;
    let edge = if removing { DANGER } else { ACCENT };
    let title = if removing { " remove " } else { " install " };

    let w = 66.min(area.width);
    let h = 12.min(area.height);
    let a = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, a);
    let block = Block::bordered()
        .title(Span::styled(
            title,
            Style::default().fg(INK).bg(edge).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(edge))
        .style(Style::default().bg(PANEL));

    let (verb, where_, extra) = if removing {
        (
            "Remove",
            format!("runs  sudo pacman -Rns {}", p.name),
            "also drops dependencies nothing else needs".to_string(),
        )
    } else {
        let route = if p.src == Src::Aur {
            "via your AUR helper".to_string()
        } else {
            "from the official repos (sudo pacman -S --needed)".to_string()
        };
        ("Install", route, String::new())
    };

    let mut lines = vec![
        Line::from(vec![
            Span::raw(format!("  {verb} ")),
            Span::styled(
                p.name.clone(),
                Style::default().fg(edge).add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]),
        Line::from(Span::styled(
            format!("  {where_}"),
            Style::default().fg(DIM),
        )),
    ];
    if !extra.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {extra}"),
            Style::default().fg(FAINT),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        key("y"),
        lbl("yes"),
        sep(),
        key("n"),
        lbl("cancel"),
    ]));
    f.render_widget(Paragraph::new(lines).block(block), a);
}
