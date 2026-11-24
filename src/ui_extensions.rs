//! Extensions sidebar: one shared set of rendered hit regions for mouse and keyboard navigation.
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{ActivePane, App, SidebarView},
    extensions::{ExtensionRecordView, ExtensionState},
    text_input::TextInputId,
    ui_widgets::InputField,
};

pub(crate) const CARD_HEIGHT: u16 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Install,
    Refresh,
    ReloadAll,
    Clear,
    Filter(&'static str),
    /// Fold or unfold one of the [`SECTIONS`].
    Collapse(usize),
    /// Install the newest release of a marketplace listing.
    InstallListing(usize),
    /// Choose which release of a marketplace listing to install.
    ChooseVersion(usize),
    /// Update an installed extension (by view index) to its newest release.
    Update(usize),
    /// Uninstall an editor-installed extension (by view index); asks to confirm first.
    Uninstall(usize),
    UpdateAll,
}

pub(crate) struct Sidebar {
    pub search_focused: bool,
    /// Folded state of each of the [`SECTIONS`].
    pub collapsed: [bool; SECTIONS],
    search_area: Rect,
    pub list_area: Rect,
    pub rows: Vec<(usize, Rect)>,
    pub actions: Vec<(Action, Rect)>,
    last_click: Option<(usize, std::time::Instant)>,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            search_focused: true,
            collapsed: [false; SECTIONS],
            search_area: Rect::default(),
            list_area: Rect::default(),
            rows: vec![],
            actions: vec![],
            last_click: None,
        }
    }
}

pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

/// Clip by terminal cells, keeping emoji and combining sequences intact.
pub(crate) fn clip(text: &str, width: u16) -> String {
    let text = clean(text);
    if text.width() <= usize::from(width) {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        if used + grapheme.width() > usize::from(width - 1) {
            break;
        }
        used += grapheme.width();
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

pub(crate) fn publisher(ext: &ExtensionRecordView) -> &str {
    ext.id
        .split_once('.')
        .map_or("Local extension", |(publisher, _)| publisher)
}

pub(crate) fn status(ext: &ExtensionRecordView) -> &'static str {
    match ext.state {
        ExtensionState::Loaded => "Enabled",
        ExtensionState::Error => "Failed",
        ExtensionState::Unloaded => "Disabled",
    }
}

/// Sidebar sections, in display order.
pub(crate) const GROUPS: [(ExtensionState, &str); 3] = [
    (ExtensionState::Loaded, "INSTALLED"),
    (ExtensionState::Unloaded, "DISABLED"),
    (ExtensionState::Error, "ERRORS"),
];

/// The marketplace section follows the installed ones.
pub(crate) const MARKETPLACE: usize = GROUPS.len();
pub(crate) const SECTIONS: usize = GROUPS.len() + 1;

/// Selection keys at and above this are marketplace listings, not installed extensions.
pub(crate) const LISTING: usize = 1 << 24;

fn section_label(section: usize) -> &'static str {
    GROUPS
        .get(section)
        .map_or("MARKETPLACE", |(_, label)| label)
}

pub(crate) fn group_of(state: ExtensionState) -> usize {
    GROUPS
        .iter()
        .position(|(group, _)| *group == state)
        .unwrap_or(0)
}

/// One row of the sidebar list: a section header or an extension card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Header {
        group: usize,
        matched: usize,
        total: usize,
    },
    Card(usize),
    /// A marketplace listing that is not installed.
    Listing(usize),
    /// Why the marketplace section shows no listings (loading, failed, …).
    Notice(Notice),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    Loading,
    Failed,
    AllInstalled,
    Empty,
}

impl Item {
    pub(crate) fn height(self) -> u16 {
        match self {
            Item::Header { .. } => 1,
            Item::Notice(_) => 3,
            Item::Card(_) | Item::Listing(_) => CARD_HEIGHT,
        }
    }

    /// The selection key of a card: a view index, or [`LISTING`] + listing index.
    pub(crate) fn card(self) -> Option<usize> {
        match self {
            Item::Card(index) => Some(index),
            Item::Listing(index) => Some(LISTING + index),
            Item::Header { .. } | Item::Notice(_) => None,
        }
    }
}

/// Matching extension indices, grouped by section, including folded ones.
pub(crate) fn grouped(views: &[ExtensionRecordView], query: &str) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..views.len())
        .filter(|index| matches(&views[*index], query))
        .collect();
    indices.sort_by_key(|index| group_of(views[*index].state));
    indices
}

/// The list rows: a header per non-empty section, then its cards unless folded.
pub(crate) fn items(app: &App) -> Vec<Item> {
    let views = app.extension_views();
    let updates_only = app.extensions_filter.to_lowercase().contains("@updates");
    let mut items = Vec::new();
    for (group, (state, _)) in GROUPS.iter().enumerate() {
        let total = views.iter().filter(|ext| ext.state == *state).count();
        let cards: Vec<usize> = (0..views.len())
            .filter(|index| {
                views[*index].state == *state
                    && matches(&views[*index], &app.extensions_filter)
                    && (!updates_only || app.extension_update(&views[*index]).is_some())
            })
            .collect();
        if cards.is_empty() {
            continue;
        }
        items.push(Item::Header {
            group,
            matched: cards.len(),
            total,
        });
        if !app.ext_sidebar.collapsed[group] {
            items.extend(cards.into_iter().map(Item::Card));
        }
    }
    let listings = app.marketplace_listings();
    let cards = listing_indices(&listings, &app.extensions_filter);
    let query = app.extensions_filter.to_lowercase();
    let installed_only = query.split_whitespace().any(|word| {
        matches!(
            word,
            "@installed" | "@enabled" | "@disabled" | "@error" | "@errors" | "@updates"
        )
    });
    let catalog = &app.ext_catalog;
    // The section never silently disappears while it loads or after it fails.
    let notice = if !cards.is_empty() || installed_only {
        None
    } else if catalog.refreshing {
        Some(Notice::Loading)
    } else if catalog.catalog.listings.is_empty() && !catalog.catalog.errors.is_empty() {
        Some(Notice::Failed)
    } else if query.split_whitespace().all(|word| word == "@marketplace") {
        Some(if catalog.catalog.listings.is_empty() {
            Notice::Empty
        } else {
            Notice::AllInstalled
        })
    } else {
        None
    };
    if !cards.is_empty() || notice.is_some() {
        items.push(Item::Header {
            group: MARKETPLACE,
            matched: cards.len(),
            total: listings.len(),
        });
        if !app.ext_sidebar.collapsed[MARKETPLACE] {
            items.extend(cards.into_iter().map(Item::Listing));
            items.extend(notice.map(Item::Notice));
        }
    }
    items
}

/// Marketplace listings matching the search, folded ones included.
pub(crate) fn listing_indices(
    listings: &[crate::extension_catalog::Listing],
    query: &str,
) -> Vec<usize> {
    (0..listings.len())
        .filter(|index| listing_matches(&listings[*index], query))
        .collect()
}

pub(crate) fn listing_matches(listing: &crate::extension_catalog::Listing, query: &str) -> bool {
    let haystack = format!(
        "{} {} {} {} {}",
        listing.name, listing.description, listing.id, listing.repository, listing.marketplace
    )
    .to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| match word {
            "@marketplace" => true,
            "@installed" | "@enabled" | "@disabled" | "@error" | "@errors" | "@updates" => false,
            _ => haystack.contains(word),
        })
}

/// Every selectable key matching the search, in sidebar order, folded sections included.
pub(crate) fn selectable(app: &App) -> Vec<usize> {
    let mut keys = grouped(&app.extension_views(), &app.extensions_filter);
    if app.extensions_filter.to_lowercase().contains("@updates") {
        let views = app.extension_views();
        keys.retain(|index| app.extension_update(&views[*index]).is_some());
    }
    keys.extend(
        listing_indices(&app.marketplace_listings(), &app.extensions_filter)
            .into_iter()
            .map(|index| LISTING + index),
    );
    keys
}

/// The first item index from which the rest of the list fits in `height`.
pub(crate) fn max_scroll(items: &[Item], height: u16) -> usize {
    if height == 0 {
        return items.len().saturating_sub(1);
    }
    let mut used = 0u16;
    for (position, item) in items.iter().enumerate().rev() {
        used = used.saturating_add(item.height());
        if used > height {
            return position + 1;
        }
    }
    0
}

/// Scroll so the selected card, and its section header when it leads one, is visible.
pub(crate) fn scroll_to_selected(app: &mut App, items: &[Item]) {
    let Some(position) = items
        .iter()
        .position(|item| *item == Item::Card(app.extensions_selected))
    else {
        return;
    };
    let height = app.ext_sidebar.list_area.height;
    let top = if position > 0 && items[position - 1].card().is_none() {
        position - 1
    } else {
        position
    };
    if top < app.extensions_scroll {
        app.extensions_scroll = top;
    }
    while app.extensions_scroll < position
        && items[app.extensions_scroll..=position]
            .iter()
            .map(|item| item.height())
            .sum::<u16>()
            > height
    {
        app.extensions_scroll += 1;
    }
}

pub(crate) fn matches(ext: &ExtensionRecordView, query: &str) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        ext.name, ext.description, ext.id, ext.repository
    )
    .to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| match word {
            "@installed" => true,
            "@enabled" => ext.state == ExtensionState::Loaded,
            "@disabled" => ext.state == ExtensionState::Unloaded,
            "@error" | "@errors" => ext.state == ExtensionState::Error,
            "@marketplace" => false,
            // Narrowed further by `selectable`, which knows the update catalog.
            "@updates" => true,
            _ => haystack.contains(word),
        })
}

fn button(f: &mut Frame, app: &mut App, area: Rect, label: &str, action: Action, selected: bool) {
    if area.is_empty() {
        return;
    }
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let style = if selected {
        crate::ui_widgets::action_button_style(
            &app.colors,
            app.colors.bg_sidebar,
            true,
            hovered,
            true,
        )
    } else {
        app.colors.ui.toolbar_layer_style(
            app.colors.ui.sidebar_foreground,
            app.colors.bg_sidebar,
            hovered,
            false,
            false,
        )
    };
    f.render_widget(Paragraph::new(clip(label, area.width)).style(style), area);
    app.ext_sidebar.actions.push((action, area));
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    app.ext_sidebar.rows.clear();
    app.ext_sidebar.actions.clear();
    app.ext_sidebar.list_area = Rect::default();
    app.ext_sidebar.search_area = Rect::default();
    app.remove_text_input_regions(|id| id == TextInputId::ExtensionsFilter);
    if area.is_empty() {
        return;
    }
    app.start_empty_window_extension_services();
    app.ensure_extension_catalog();
    let bg = app.colors.bg_sidebar;
    let fg = app.colors.ui.sidebar_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    let base = Style::default().fg(fg).bg(bg);
    f.render_widget(Block::default().style(base), area);
    let inset = u16::from(area.width > 4);
    let inner = Rect::new(
        area.x + inset,
        area.y,
        area.width.saturating_sub(inset * 2),
        area.height,
    );
    let install = if inner.width >= 29 {
        " + Install "
    } else {
        " + "
    };
    let views = app.extension_views();
    let updates = views
        .iter()
        .filter(|view| app.extension_update(view).is_some())
        .count();
    let update_all = format!(" ↑{updates} ");
    let mut header_buttons = vec![(install, Action::Install), (" ⟳ ", Action::ReloadAll)];
    if updates > 0 {
        header_buttons.insert(0, (update_all.as_str(), Action::UpdateAll));
    }
    let button_width: u16 = header_buttons
        .iter()
        .map(|(label, _)| label.width() as u16)
        .sum();
    let title_width = inner.width.saturating_sub(button_width + 1);
    f.render_widget(
        Paragraph::new(clip("EXTENSIONS", title_width))
            .style(base.fg(dim).add_modifier(Modifier::BOLD)),
        Rect::new(inner.x, inner.y, title_width, 1),
    );
    let mut x = inner.right().saturating_sub(button_width.min(inner.width));
    for (label, action) in header_buttons {
        let width = (label.width() as u16).min(inner.right().saturating_sub(x));
        button(
            f,
            app,
            Rect::new(x, inner.y, width, 1),
            label,
            action,
            false,
        );
        x += width;
    }
    if inner.height < 2 {
        return;
    }
    let roomy = inner.height >= 15;
    let search_y = inner.y + if roomy { 2 } else { 1 };
    let search_h = if roomy { 3 } else { 1 };
    let search = Rect::new(
        inner.x,
        search_y,
        inner.width,
        search_h.min(inner.bottom().saturating_sub(search_y)),
    );
    let focused = app.ext_sidebar.search_focused
        && app.active_pane == ActivePane::Explorer
        && app.sidebar_view == SidebarView::Extensions;
    app.ext_sidebar.search_area = search;
    let input_style = app.colors.ui.input_style(bg);
    let input_bg = input_style.bg.unwrap_or(bg);
    let border = if focused {
        app.colors.ui.focus_border.border(input_bg)
    } else {
        app.colors.ui.input_colors.border.border(input_bg)
    };
    let block = Block::default()
        .borders(if roomy { Borders::ALL } else { Borders::NONE })
        .style(input_style)
        .border_style(Style::default().fg(border.unwrap_or(input_bg)).bg(input_bg));
    let mut field = block.inner(search);
    f.render_widget(block, search);
    let clear = if !app.extensions_filter.is_empty() && field.width >= 8 {
        let rect = Rect::new(field.right() - 3, field.y, 3, 1);
        field.width -= 3;
        Some(rect)
    } else {
        None
    };
    if !field.is_empty() {
        let hit = InputField::new(&app.extensions_filter)
            .viewport(app.text_input_viewport(TextInputId::ExtensionsFilter))
            .theme_colors(&app.colors)
            .prefix(" ⌕ ", app.colors.ui.icon_color(input_bg))
            .placeholder_text("Search extensions and marketplace")
            .cursor_marker(if focused && app.cursor_visible {
                "│"
            } else {
                ""
            })
            .cursor_pos(app.extensions_filter_cursor)
            .selection_range(App::inline_selection_range(
                app.extensions_filter_cursor,
                app.extensions_filter_selection_anchor,
            ))
            .bg(input_bg)
            .render(f, field);
        app.register_text_input(TextInputId::ExtensionsFilter, hit);
    }
    if let Some(rect) = clear {
        button(f, app, rect, " × ", Action::Clear, false);
    }
    let mut y = search.bottom();
    if inner.height >= 10 && inner.width >= 25 {
        y += u16::from(roomy);
        x = inner.x;
        let filters = if inner.width >= 35 {
            [
                (" All ", ""),
                (" Enabled ", "@enabled"),
                (" Disabled ", "@disabled"),
                (" Errors ", "@error"),
            ]
        } else {
            [
                (" All ", ""),
                (" On ", "@enabled"),
                (" Off ", "@disabled"),
                (" Errors ", "@error"),
            ]
        };
        for (label, token) in filters {
            let width = label.width() as u16;
            let selected = if token.is_empty() {
                !app.extensions_filter
                    .split_whitespace()
                    .any(|w| w.starts_with('@'))
            } else {
                app.extensions_filter
                    .split_whitespace()
                    .any(|w| w.eq_ignore_ascii_case(token))
            };
            button(
                f,
                app,
                Rect::new(x, y, width, 1),
                label,
                Action::Filter(token),
                selected,
            );
            x += width + 1;
        }
        y += 2;
    }
    if y >= inner.bottom() {
        return;
    }
    app.clamp_extensions_selection();
    let listings = app.marketplace_listings();
    let any_match = !items(app).is_empty();
    let footer_h = u16::from(inner.bottom().saturating_sub(y) >= 7);
    let list = Rect::new(
        inner.x,
        y,
        inner.width,
        inner.bottom().saturating_sub(y + footer_h),
    );
    app.ext_sidebar.list_area = list;
    if !any_match {
        let empty = if views.is_empty()
            && listings.is_empty()
            && (app.bg_extension_scan_running || app.ext_catalog.refreshing)
        {
            "Loading extensions…"
        } else if views.is_empty() {
            "Build your workspace

Install an extension from a tagged GitHub release to add tools and language support."
        } else {
            "No extensions found

Try another search or clear your filters."
        };
        f.render_widget(
            Paragraph::new(empty)
                .style(base.fg(dim))
                .wrap(Wrap { trim: true }),
            Rect::new(
                inner.x,
                y.saturating_add(1).min(list.bottom()),
                inner.width,
                list.height.saturating_sub(1),
            ),
        );
        let label = if views.is_empty() {
            " + Install from GitHub "
        } else {
            " Clear search "
        };
        let action = if views.is_empty() {
            Action::Install
        } else {
            Action::Clear
        };
        if list.height >= 7 {
            button(
                f,
                app,
                Rect::new(
                    inner.x,
                    list.bottom() - 2,
                    (label.width() as u16).min(inner.width),
                    1,
                ),
                label,
                action,
                true,
            );
        }
    } else {
        let items = items(app);
        app.extensions_scroll = app.extensions_scroll.min(max_scroll(&items, list.height));
        let scroll = app.extensions_scroll;
        let mut row_y = list.y;
        for item in &items[scroll..] {
            if row_y >= list.bottom() {
                break;
            }
            let height = item.height().min(list.bottom() - row_y);
            match *item {
                Item::Header {
                    group,
                    matched,
                    total,
                } => draw_header(
                    f,
                    app,
                    Rect::new(list.x, row_y, list.width, 1),
                    group,
                    matched,
                    total,
                ),
                Item::Notice(notice) => {
                    draw_notice(f, app, Rect::new(list.x, row_y, list.width, height), notice)
                }
                Item::Listing(index) => draw_listing(
                    f,
                    app,
                    Rect::new(list.x, row_y, list.width, height),
                    index,
                    &listings[index],
                    bg,
                    fg,
                ),
                Item::Card(index) => {
                    draw_card(
                        f,
                        app,
                        Rect::new(list.x, row_y, list.width, height),
                        index,
                        &views[index],
                        bg,
                        fg,
                    );
                }
            }
            row_y += height;
        }
        let total: u16 = items.iter().map(|item| item.height()).sum();
        if total > list.height && list.width > 0 && list.height > 0 {
            let offset: u16 = items[..scroll].iter().map(|item| item.height()).sum();
            let thumb_h =
                (u32::from(list.height) * u32::from(list.height) / u32::from(total)).max(1) as u16;
            let max_offset: u16 = items[..max_scroll(&items, list.height)]
                .iter()
                .map(|item| item.height())
                .sum();
            let thumb_y = list.y
                + (u32::from(list.height - thumb_h) * u32::from(offset)
                    / u32::from(max_offset.max(1)))
                .min(u32::from(list.height - thumb_h)) as u16;
            let scrollbar_fg = app.colors.ui.description_color(bg);
            f.render_widget(
                Paragraph::new(
                    "▏
"
                    .repeat(usize::from(thumb_h)),
                )
                .style(base.fg(scrollbar_fg)),
                Rect::new(list.right() - 1, thumb_y, 1, thumb_h),
            );
        }
    }
    if footer_h > 0 {
        f.render_widget(
            Paragraph::new(clip(
                if app.extensions_selected >= LISTING {
                    "↑↓ Browse  Enter Install  Tab Search"
                } else {
                    "↑↓ Browse  Enter Open  Tab Search"
                },
                inner.width,
            ))
            .style(base.fg(dim)),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}

fn draw_header(
    f: &mut Frame,
    app: &mut App,
    section: Rect,
    group: usize,
    matched: usize,
    total: usize,
) {
    let bg = app.colors.bg_sidebar;
    let dim = app.colors.ui.description_color(bg);
    let style = app
        .colors
        .ui
        .toolbar_layer_style(
            app.colors.ui.section_foreground,
            bg,
            section.contains((app.mouse_col, app.mouse_row).into()),
            false,
            false,
        )
        .add_modifier(Modifier::BOLD);
    let arrow = if app.ext_sidebar.collapsed[group] {
        "›"
    } else {
        "⌄"
    };
    f.render_widget(
        Paragraph::new(clip(
            &format!("{arrow} {}", section_label(group)),
            section.width,
        ))
        .style(style),
        section,
    );
    if group == MARKETPLACE {
        draw_marketplace_check(f, app, section, &style);
        app.ext_sidebar
            .actions
            .push((Action::Collapse(group), section));
        return;
    }
    let catalog = &app.ext_catalog;
    let count = if group == MARKETPLACE && catalog.refreshing {
        "loading…".to_string()
    } else if group == MARKETPLACE && !catalog.catalog.errors.is_empty() {
        "offline".to_string()
    } else if group == MARKETPLACE && total == 0 {
        String::new()
    } else if matched == total {
        format!("{total}")
    } else {
        format!("{matched} / {total}")
    };
    let count_width = count.width() as u16;
    if count_width + 12 <= section.width {
        f.render_widget(
            Paragraph::new(count).style(style.fg(dim).remove_modifier(Modifier::BOLD)),
            Rect::new(section.right() - count_width, section.y, count_width, 1),
        );
    }
    app.ext_sidebar
        .actions
        .push((Action::Collapse(group), section));
}

/// The MARKETPLACE header's "Check for updates" button: rescans extension
/// folders, then asks the marketplaces and releases for updates. While a
/// check runs it reads "Checking…" and does nothing when pressed.
fn draw_marketplace_check(f: &mut Frame, app: &mut App, section: Rect, header: &Style) {
    let catalog = &app.ext_catalog;
    let checking = catalog.refreshing;
    let offline = !checking && !catalog.catalog.errors.is_empty();
    let title = format!("⌄ {}", section_label(MARKETPLACE)).width() as u16 + 2;
    let room = section.width.saturating_sub(title);
    let label = [
        if checking {
            " Checking… "
        } else if offline {
            " Offline · Retry "
        } else {
            " Check for updates "
        },
        if checking {
            " … "
        } else if offline {
            " Retry "
        } else {
            " Check "
        },
    ]
    .into_iter()
    .find(|label| label.width() as u16 <= room);
    let Some(label) = label else {
        return;
    };
    let width = label.width() as u16;
    let area = Rect::new(section.right() - width, section.y, width, 1);
    let hovered = !checking && area.contains((app.mouse_col, app.mouse_row).into());
    let style = crate::ui_widgets::filled_button_style(
        &app.colors,
        header.bg.unwrap_or(app.colors.bg_sidebar),
        false,
        hovered,
        !checking,
    );
    f.render_widget(Paragraph::new(label).style(style), area);
    if !checking {
        app.ext_sidebar.actions.push((Action::Refresh, area));
    }
}

fn draw_card(
    f: &mut Frame,
    app: &mut App,
    card: Rect,
    index: usize,
    ext: &ExtensionRecordView,
    bg: ratatui::style::Color,
    fg: ratatui::style::Color,
) {
    let selected = index == app.extensions_selected;
    let hovered = card.contains((app.mouse_col, app.mouse_row).into());
    let row_style = app.colors.ui.list_row_style(
        app.colors.ui.sidebar_foreground,
        bg,
        selected,
        app.active_pane == ActivePane::Explorer,
        hovered,
    );
    let style = row_style.remove_modifier(Modifier::UNDERLINED);
    let row_bg = style.bg.unwrap_or(bg);
    let row_fg = style.fg.unwrap_or(fg);
    let secondary = if selected {
        row_fg
    } else {
        app.colors.ui.description_color(row_bg)
    };
    f.render_widget(Block::default().style(style), card);
    let wide = card.width >= 24;
    let text_x = card.x + if wide { 6 } else { 1 };
    let text_width = card.right().saturating_sub(text_x + 2);
    if wide && card.height >= 3 {
        let initial = ext
            .name
            .graphemes(true)
            .find(|g| g.width() == 1 && !g.trim().is_empty())
            .unwrap_or("+");
        let badge = format!("┌─┐\n│{initial}│\n└─┘");
        f.render_widget(
            Paragraph::new(badge).style(style.fg(secondary)),
            Rect::new(card.x + 1, card.y, 3, 3),
        );
    }
    let version = format!("v{}", clean(&ext.version));
    let version_w = if text_width >= 30 {
        (version.width() as u16).min(text_width / 3)
    } else {
        0
    };
    f.render_widget(
        Paragraph::new(clip(
            &ext.name,
            text_width.saturating_sub(version_w + u16::from(version_w > 0)),
        ))
        .style(style.add_modifier(Modifier::BOLD)),
        Rect::new(text_x, card.y, text_width.saturating_sub(version_w), 1),
    );
    if version_w > 0 {
        f.render_widget(
            Paragraph::new(clip(&version, version_w)).style(style.fg(secondary)),
            Rect::new(text_x + text_width - version_w, card.y, version_w, 1),
        );
    }
    if card.height >= 2 {
        let description = if ext.description.is_empty() {
            "No description provided"
        } else {
            &ext.description
        };
        let description_fg = if ext.last_error.is_some() && !selected {
            app.colors.ui.list_error_color(row_bg)
        } else {
            secondary
        };
        f.render_widget(
            Paragraph::new(clip(description, text_width)).style(
                style.fg(app
                    .colors
                    .ui
                    .readable_foreground(description_fg, row_bg, 4.5)),
            ),
            Rect::new(text_x, card.y + 1, text_width, 1),
        );
    }
    if card.height >= 3 {
        let icon = match ext.state {
            ExtensionState::Loaded => "●",
            ExtensionState::Error => "✖",
            ExtensionState::Unloaded => "○",
        };
        let color = match ext.state {
            ExtensionState::Loaded => app.colors.fg_success,
            ExtensionState::Error => app.colors.ui.list_error_color(row_bg),
            ExtensionState::Unloaded => secondary,
        };
        let armed = app.ext_info.remove_armed.as_deref() == Some(ext.id.as_str());
        // Always offered: the section header already tells enabled, disabled and failed apart.
        let uninstall = ext.removable;
        let state = if uninstall {
            if armed { " Confirm? " } else { " Uninstall " }.to_string()
        } else {
            format!("{icon} {}", status(ext))
        };
        let state_w = (state.width() as u16).min(text_width);
        let mut publisher_w = text_width.saturating_sub(state_w + 2);
        let installing = app.extension_installing(&ext.repository);
        let label = if installing {
            Some(" Updating… ".to_string())
        } else {
            app.extension_update(ext)
                .map(|update| format!(" ↑ {} ", clean(&update.name)))
        };
        if let Some(label) = label {
            let width = label.width() as u16;
            if width + state_w < text_width {
                let rect = Rect::new(
                    text_x + text_width - state_w - 1 - width,
                    card.y + 2,
                    width,
                    1,
                );
                if installing {
                    f.render_widget(Paragraph::new(label).style(style.fg(secondary)), rect);
                } else {
                    button(f, app, rect, &label, Action::Update(index), true);
                }
                publisher_w = publisher_w.saturating_sub(width + 1);
            }
        }
        f.render_widget(
            Paragraph::new(clip(publisher(ext), publisher_w)).style(style.fg(secondary)),
            Rect::new(text_x, card.y + 2, publisher_w, 1),
        );
        let state_rect = Rect::new(text_x + text_width - state_w, card.y + 2, state_w, 1);
        if uninstall {
            // Filled like Install, so it reads as a button and not as a label.
            button(f, app, state_rect, &state, Action::Uninstall(index), true);
        } else {
            f.render_widget(
                Paragraph::new(clip(&state, state_w))
                    .style(style.fg(app.colors.ui.readable_foreground(color, row_bg, 4.5))),
                state_rect,
            );
        }
    }
    if selected {
        f.render_widget(
            Paragraph::new("▏\n".repeat(usize::from(card.height)))
                .style(style.fg(row_style.underline_color.unwrap_or(row_fg))),
            Rect::new(card.x, card.y, 1, card.height),
        );
    }
    app.ext_sidebar.rows.push((index, card));
}

/// The marketplace section's status line when it has no cards to show.
fn draw_notice(f: &mut Frame, app: &mut App, area: Rect, notice: Notice) {
    let bg = app.colors.bg_sidebar;
    let dim = app.colors.ui.description_color(bg);
    let style = Style::default().fg(dim).bg(bg);
    let x = area.x + 2;
    let width = area.width.saturating_sub(3);
    let (message, detail) = match notice {
        Notice::Loading => ("Loading marketplace…".to_string(), None),
        Notice::Failed => (
            "Couldn't load the marketplace. Press ↻ to retry.".to_string(),
            app.ext_catalog.catalog.errors.first().cloned(),
        ),
        Notice::AllInstalled => (
            "Every marketplace extension is installed.".to_string(),
            None,
        ),
        Notice::Empty => ("The marketplace lists no extensions.".to_string(), None),
    };
    let message_style = if notice == Notice::Failed {
        style.fg(app.colors.ui.list_error_color(bg))
    } else {
        style
    };
    f.render_widget(
        Paragraph::new(clip(&message, width)).style(message_style),
        Rect::new(x, area.y, width, 1.min(area.height)),
    );
    if let Some(detail) = detail
        && area.height >= 2
    {
        f.render_widget(
            Paragraph::new(clean(&detail))
                .style(style)
                .wrap(Wrap { trim: true }),
            Rect::new(x, area.y + 1, width, area.height - 1),
        );
    }
}

/// A marketplace card: like an installed one, with Install and a version menu.
fn draw_listing(
    f: &mut Frame,
    app: &mut App,
    card: Rect,
    index: usize,
    listing: &crate::extension_catalog::Listing,
    bg: ratatui::style::Color,
    fg: ratatui::style::Color,
) {
    let key = LISTING + index;
    let selected = key == app.extensions_selected;
    let hovered = card.contains((app.mouse_col, app.mouse_row).into());
    let row_style = app.colors.ui.list_row_style(
        app.colors.ui.sidebar_foreground,
        bg,
        selected,
        app.active_pane == ActivePane::Explorer,
        hovered,
    );
    let style = row_style.remove_modifier(Modifier::UNDERLINED);
    let row_bg = style.bg.unwrap_or(bg);
    let row_fg = style.fg.unwrap_or(fg);
    let secondary = if selected {
        row_fg
    } else {
        app.colors.ui.description_color(row_bg)
    };
    f.render_widget(Block::default().style(style), card);
    let wide = card.width >= 24;
    let text_x = card.x + if wide { 6 } else { 1 };
    let text_width = card.right().saturating_sub(text_x + 2);
    if wide && card.height >= 3 {
        let initial = listing
            .name
            .graphemes(true)
            .find(|g| g.width() == 1 && !g.trim().is_empty())
            .unwrap_or("+");
        f.render_widget(
            Paragraph::new(format!("┌─┐\n│{initial}│\n└─┘")).style(style.fg(secondary)),
            Rect::new(card.x + 1, card.y, 3, 3),
        );
    }
    f.render_widget(
        Paragraph::new(clip(&listing.name, text_width)).style(style.add_modifier(Modifier::BOLD)),
        Rect::new(text_x, card.y, text_width, 1),
    );
    if card.height >= 2 {
        let description = if listing.description.is_empty() {
            &listing.repository
        } else {
            &listing.description
        };
        f.render_widget(
            Paragraph::new(clip(description, text_width))
                .style(style.fg(app.colors.ui.readable_foreground(secondary, row_bg, 4.5))),
            Rect::new(text_x, card.y + 1, text_width, 1),
        );
    }
    if card.height >= 3 {
        let mut right = text_x + text_width;
        if app.extension_installing(&listing.repository) {
            let label = "Installing…";
            let width = (label.width() as u16).min(text_width);
            right -= width;
            f.render_widget(
                Paragraph::new(label).style(style.fg(secondary)),
                Rect::new(right, card.y + 2, width, 1),
            );
        } else if text_width >= 14 {
            right -= 3;
            button(
                f,
                app,
                Rect::new(right, card.y + 2, 3, 1),
                "▏▾ ",
                Action::ChooseVersion(index),
                true,
            );
            right -= 9;
            button(
                f,
                app,
                Rect::new(right, card.y + 2, 9, 1),
                " Install ",
                Action::InstallListing(index),
                true,
            );
        }
        let publisher = listing
            .id
            .split_once('.')
            .map_or(listing.marketplace.as_str(), |(publisher, _)| publisher);
        let publisher = if listing.official {
            format!("{publisher} ✓")
        } else {
            format!("{publisher} · {}", listing.marketplace)
        };
        let publisher_w = right.saturating_sub(text_x + 1);
        f.render_widget(
            Paragraph::new(clip(&publisher, publisher_w)).style(style.fg(secondary)),
            Rect::new(text_x, card.y + 2, publisher_w, 1),
        );
    }
    if selected {
        f.render_widget(
            Paragraph::new("▏\n".repeat(usize::from(card.height)))
                .style(style.fg(row_style.underline_color.unwrap_or(row_fg))),
            Rect::new(card.x, card.y, 1, card.height),
        );
    }
    app.ext_sidebar.rows.push((key, card));
}

fn install_listing(app: &mut App, index: usize, choose: bool) {
    if let Some(listing) = app.marketplace_listings().get(index) {
        if choose {
            app.choose_extension_version(&listing.repository);
        } else {
            app.install_latest_extension(&listing.repository);
        }
    }
}

pub(crate) fn handle_click(app: &mut App, col: u16, row: u16) {
    app.active_pane = ActivePane::Explorer;
    // Any other click cancels a pending uninstall confirmation.
    let armed = app.ext_info.remove_armed.take();
    if let Some((action, _)) = app
        .ext_sidebar
        .actions
        .iter()
        .find(|(_, rect)| rect.contains((col, row).into()))
        .copied()
    {
        app.ext_sidebar.last_click = None;
        match action {
            Action::Install => app.open_extension_install_prompt(),
            Action::Refresh => {
                app.extensions_rescan();
                app.check_extension_updates();
            }
            Action::InstallListing(index) => install_listing(app, index, false),
            Action::ChooseVersion(index) => install_listing(app, index, true),
            Action::Update(index) => {
                if let Some(view) = app.extension_views().get(index) {
                    app.install_latest_extension(&view.repository);
                }
            }
            Action::UpdateAll => app.update_all_extensions(),
            Action::Uninstall(index) => {
                if let Some(view) = app.extension_views().get(index) {
                    app.ext_info.remove_armed = armed;
                    app.uninstall_extension(&view.id);
                }
            }
            Action::ReloadAll => app.extensions_reload_all(),
            Action::Clear => {
                app.extensions_filter.clear();
                app.extensions_filter_cursor = 0;
                app.extensions_filter_selection_anchor = None;
                app.extensions_scroll = 0;
                app.ext_sidebar.search_focused = true;
                app.clamp_extensions_selection();
            }
            Action::Filter(token) => {
                let words: Vec<_> = app
                    .extensions_filter
                    .split_whitespace()
                    .filter(|word| {
                        !matches!(
                            word.to_ascii_lowercase().as_str(),
                            "@installed"
                                | "@enabled"
                                | "@disabled"
                                | "@error"
                                | "@errors"
                                | "@marketplace"
                                | "@updates"
                        )
                    })
                    .collect();
                app.extensions_filter = words
                    .into_iter()
                    .chain((!token.is_empty()).then_some(token))
                    .collect::<Vec<_>>()
                    .join(" ");
                app.extensions_filter_cursor = app.extensions_filter.chars().count();
                app.extensions_filter_selection_anchor = None;
                app.extensions_scroll = 0;
                app.ext_sidebar.collapsed = [false; SECTIONS];
                app.clamp_extensions_selection();
            }
            Action::Collapse(group) => {
                app.ext_sidebar.collapsed[group] = !app.ext_sidebar.collapsed[group];
                app.clamp_extensions_selection();
            }
        }
        return;
    }
    if app.ext_sidebar.search_area.contains((col, row).into()) {
        app.ext_sidebar.search_focused = true;
        return;
    }
    if let Some((index, _)) = app
        .ext_sidebar
        .rows
        .iter()
        .find(|(_, rect)| rect.contains((col, row).into()))
        .copied()
    {
        let now = std::time::Instant::now();
        let double = app.ext_sidebar.last_click.is_some_and(|(previous, time)| {
            previous == index && now.duration_since(time).as_millis() < 400
        });
        app.extensions_selected = index;
        app.ext_sidebar.search_focused = false;
        if index >= LISTING {
            if double {
                install_listing(app, index - LISTING, false);
            }
        } else if let Some(ext) = app.extension_views().get(index) {
            if double {
                app.open_extension_info_tab(&ext.id);
            } else {
                app.preview_extension_info_tab(&ext.id);
            }
        }
        app.active_pane = ActivePane::Explorer;
        app.ext_sidebar.last_click = Some((index, now));
    }
}

fn preview_selected(app: &mut App) {
    if let Some(ext) = app
        .extension_views()
        .get(app.extensions_selected)
        .filter(|ext| matches(ext, &app.extensions_filter))
    {
        app.preview_extension_info_tab(&ext.id);
        app.active_pane = ActivePane::Explorer;
    }
}

pub(crate) fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
            app.ext_sidebar.search_focused = false;
            // With every section folded, browsing unfolds them rather than doing nothing.
            if !items(app).iter().any(|item| item.card().is_some()) {
                app.ext_sidebar.collapsed = [false; SECTIONS];
            }
            let page = isize::try_from((app.ext_sidebar.list_area.height / CARD_HEIGHT).max(1))
                .unwrap_or(1);
            match code {
                KeyCode::Up => app.extensions_select_prev(),
                KeyCode::Down => app.extensions_select_next(),
                KeyCode::PageUp => app.move_extensions_selection(-page),
                _ => app.move_extensions_selection(page),
            }
            preview_selected(app);
        }
        KeyCode::Home | KeyCode::End if !app.ext_sidebar.search_focused => {
            app.move_extensions_selection(if code == KeyCode::Home {
                isize::MIN
            } else {
                isize::MAX
            });
            preview_selected(app);
        }
        KeyCode::Enter if app.extensions_selected >= LISTING => {
            if selectable(app).contains(&app.extensions_selected) {
                install_listing(app, app.extensions_selected - LISTING, false);
            }
        }
        KeyCode::Enter => {
            if let Some(ext) = app
                .extension_views()
                .get(app.extensions_selected)
                .filter(|ext| matches(ext, &app.extensions_filter))
            {
                app.open_extension_info_tab(&ext.id);
            }
        }
        KeyCode::Tab | KeyCode::BackTab => {
            app.ext_sidebar.search_focused = !app.ext_sidebar.search_focused
        }
        KeyCode::Esc => {
            if !app.extensions_filter.is_empty() {
                app.extensions_filter.clear();
                app.extensions_filter_cursor = 0;
                app.extensions_filter_selection_anchor = None;
                app.extensions_scroll = 0;
                app.clamp_extensions_selection();
            }
            app.ext_sidebar.search_focused = true;
        }
        KeyCode::F(5) => app.extensions_rescan(),
        KeyCode::Delete if !app.ext_sidebar.search_focused => {
            if let Some(ext) = app
                .extension_views()
                .get(app.extensions_selected)
                .filter(|ext| ext.removable)
            {
                app.uninstall_extension(&ext.id);
            }
        }
        _ => {
            app.ext_sidebar.search_focused = true;
            if app
                .edit_text_input(TextInputId::ExtensionsFilter, code, modifiers, None)
                .changed
            {
                app.text_input_changed(TextInputId::ExtensionsFilter);
            }
        }
    }
}

pub(crate) fn draw_tooltip(f: &mut Frame, app: &App) {
    if !app.sidebar_is_visible() || app.sidebar_view != SidebarView::Extensions {
        return;
    }
    let Some((action, area)) = app
        .ext_sidebar
        .actions
        .iter()
        .find(|(_, rect)| rect.contains((app.mouse_col, app.mouse_row).into()))
    else {
        return;
    };
    let text = match action {
        Action::Install => "Install extension from a tagged GitHub release",
        Action::Refresh => "Rescan installed extensions and check marketplaces for updates",
        Action::InstallListing(_) => "Install the latest release",
        Action::ChooseVersion(_) => "Install another version…",
        Action::Update(_) => "Update to the latest release",
        Action::UpdateAll => "Update all extensions",
        Action::Uninstall(_) => "Uninstall and delete from disk (Delete)",
        Action::ReloadAll => "Reload all enabled extensions",
        Action::Clear => "Clear search and filters (Esc)",
        _ => return,
    };
    crate::ui_widgets::Tooltip::hover(vec![Line::from(Span::raw(text))], &app.colors)
        .prefer_below()
        .render(f, area.x, area.y);
}
