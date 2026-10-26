use super::{App, Rect, ExplorerScrollbar, ExplorerSection};

pub(super) struct TreeLayout {
    pub content: Rect,
    pub files: Rect,
    pub max_scroll: usize,
    pub horizontal: Option<ExplorerScrollbar>,
}

fn content_width(app: &App) -> usize {
    let visibility = app.explorer_git_badge_visibility();
    let mut width = app.explorer.content_width(visibility);
    // Unsaved indicators change without a filesystem refresh. Only inspect
    // open dirty documents, using the tree's existing path index.
    for path in app.tabs.iter().chain(app.groups.iter().flat_map(|group| &group.tabs))
        .filter(|tab| tab.has_unsaved_changes())
        .filter_map(|tab| tab.editor.file_path.as_deref())
    {
        if let Some(row_width) = app.explorer.indexed_row_width(path, visibility) {
            width = width.max(row_width.saturating_add(2));
        }
    }
    // Paragraph's horizontal offset uses terminal cells stored in a u16.
    width.min(u16::MAX as usize)
}

pub(super) fn tree_layout(app: &App, area: Rect) -> TreeLayout {
    let raw = super::explorer_tree_raw_area(app, area);
    let width = content_width(app);
    let mut vertical = false;
    let mut horizontal = false;
    // Either scrollbar can create overflow on the other axis. Resolve both
    // together; never let alternating frames add/remove the bottom row.
    for _ in 0..2 {
        vertical |= raw.width > 5 && raw.height > 0
            && app.explorer.entries.len() > usize::from(raw.height - u16::from(horizontal));
        horizontal |= raw.height > 1 && raw.width > u16::from(vertical)
            && width > usize::from(raw.width - u16::from(vertical));
    }
    let content = Rect::new(raw.x, raw.y, raw.width, raw.height - u16::from(horizontal));
    let files = Rect::new(content.x, content.y, content.width.saturating_sub(u16::from(vertical)), content.height);
    let horizontal = horizontal.then(|| {
        let length = files.width;
        let thumb = ((usize::from(length) * usize::from(length)) / width).max(1) as u16;
        let max_scroll = width - usize::from(length);
        let offset = app.explorer.horizontal.offset.min(max_scroll);
        ExplorerScrollbar {
            section: ExplorerSection::FilesHorizontal,
            area: Rect::new(files.x, content.bottom(), length, 1),
            offset,
            thumb_top: (offset as f64 / max_scroll as f64 * f64::from(length - thumb)) as u16,
            thumb_height: thumb,
            max_scroll,
        }
    });
    TreeLayout { content, files, horizontal, max_scroll: width.saturating_sub(files.width as usize) }
}

/// Consume horizontal gestures over Explorer, including its fixed headers,
/// so a sidebar gesture can never scroll the editor underneath it.
pub(crate) fn scroll_horizontal_at(app: &mut App, col: u16, row: u16, delta: isize) -> bool {
    if !app.show_explorer || app.sidebar_view != crate::app::SidebarView::Explorer { return false; }
    let Some(area) = app.explorer_area.filter(|area| area.contains((col, row).into())) else { return false; };
    if super::explorer_tree_raw_area(app, area).contains((col, row).into()) {
        let layout = tree_layout(app, area);
        let max = layout.max_scroll;
        app.explorer.horizontal.offset = app.explorer.horizontal.offset.min(max).saturating_add_signed(delta).min(max);
    }
    true
}
