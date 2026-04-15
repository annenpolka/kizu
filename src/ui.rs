use crate::app::App;

/// Render the kizu TUI frame: file list (left) + diff view (right) + footer.
///
/// TODO v0.1:
///   - ratatui::layout::Layout to split horizontally (30% / 70%)
///   - left: List of App.files with status + path + +N/-M
///   - right: Paragraph with selected file's hunks (syntect highlighted)
///   - bottom: footer with [follow] flag, current file stats, session totals
pub fn render(_app: &App) {
    // wired up in app::run loop
}
