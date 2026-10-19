use std::io::Write;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use comfy_table::{
    Attribute, Cell, Color, ContentArrangement, Row, Table, modifiers::UTF8_ROUND_CORNERS,
    presets::UTF8_FULL_CONDENSED,
};
use console::Style;
use crossterm::{
    cursor,
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, read},
    execute, queue,
    style::Print,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};

const HELP: &str = "↑/↓ move · PgUp/PgDn page · type to filter · Enter select · Esc cancel";
const DEVICE_HELP: &str =
    "↑/↓ move · PgUp/PgDn page · type to filter · Enter select · Shift+F favorite · Esc cancel";
const VIEW_HELP: &str = "↑/↓ move · PgUp/PgDn page · type filter · Shift+C copy · Esc close";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevicePick {
    Selected(usize),
    ToggleFavorite(usize),
    Cancelled,
}

/// Interactive, filterable selector that renders the candidates as a table.
/// Returns the index of the picked row, or `None` when the user cancels.
pub fn pick(title: &str, headers: &[&str], rows: Vec<Vec<String>>) -> Result<Option<usize>> {
    match pick_inner(title, headers, rows, None, 0, false)? {
        DevicePick::Selected(index) => Ok(Some(index)),
        DevicePick::Cancelled => Ok(None),
        DevicePick::ToggleFavorite(_) => unreachable!("favorites are disabled for this picker"),
    }
}

/// Device selector with a persistent favorite marker and a `Shift+F` action.
/// `favorite` and `initial_selection` are row indices in the unfiltered list.
pub fn pick_device(
    title: &str,
    headers: &[&str],
    rows: Vec<Vec<String>>,
    favorite: Option<usize>,
    initial_selection: usize,
) -> Result<DevicePick> {
    pick_inner(title, headers, rows, favorite, initial_selection, true)
}

/// Read-only table browser. The selected row can copy one of its columns to
/// the terminal clipboard using OSC 52.
pub fn view(
    title: &str,
    headers: &[&str],
    rows: Vec<Vec<String>>,
    copy_column: usize,
) -> Result<()> {
    let _guard = TtyGuard::enable()?;
    let mut picker = Picker::new(title, headers, rows, None, 0, false);
    let mut stdout = std::io::stdout();
    let mut notice = None;

    loop {
        let matching = picker.filtered();
        if picker.selected >= matching.len() {
            picker.selected = 0;
            picker.offset = 0;
        }
        let (width, height) = size()?;
        let capacity = visible_row_capacity(height);
        let (start, end) = window(picker.selected, matching.len(), capacity, picker.offset);
        picker.offset = start;
        picker.draw(
            &mut stdout,
            Frame {
                matching: &matching,
                start,
                end,
                width,
                height,
                help: VIEW_HELP,
                notice: notice.as_deref(),
            },
        )?;

        match read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Up => {
                    picker.selected = picker.selected.saturating_sub(1);
                    notice = None;
                }
                KeyCode::Down => {
                    picker.selected = picker
                        .selected
                        .saturating_add(1)
                        .min(matching.len().saturating_sub(1));
                    notice = None;
                }
                KeyCode::PageUp => {
                    picker.selected = picker.selected.saturating_sub(capacity);
                    notice = None;
                }
                KeyCode::PageDown => {
                    picker.selected = picker
                        .selected
                        .saturating_add(capacity)
                        .min(matching.len().saturating_sub(1));
                    notice = None;
                }
                KeyCode::Home => {
                    picker.selected = 0;
                    notice = None;
                }
                KeyCode::End => {
                    picker.selected = matching.len().saturating_sub(1);
                    notice = None;
                }
                KeyCode::Backspace => {
                    picker.filter.pop();
                    picker.reset_viewport();
                    notice = None;
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    picker.filter.clear();
                    picker.reset_viewport();
                    notice = None;
                }
                KeyCode::Char('C')
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if let Some(&index) = matching.get(picker.selected)
                        && let Some(value) = picker.rows[index].get(copy_column)
                    {
                        copy_to_clipboard(&mut stdout, value)?;
                        let attribute = picker.rows[index]
                            .first()
                            .map(String::as_str)
                            .unwrap_or("value");
                        notice = Some(format!("Copied {attribute}"));
                    }
                }
                KeyCode::Char(input)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    picker.filter.push(input);
                    picker.reset_viewport();
                    notice = None;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn pick_inner(
    title: &str,
    headers: &[&str],
    rows: Vec<Vec<String>>,
    favorite: Option<usize>,
    initial_selection: usize,
    allow_favorite: bool,
) -> Result<DevicePick> {
    let _guard = TtyGuard::enable()?;
    let mut picker = Picker::new(
        title,
        headers,
        rows,
        favorite,
        initial_selection,
        allow_favorite,
    );
    let mut stdout = std::io::stdout();

    loop {
        let matching = picker.filtered();
        if picker.selected >= matching.len() {
            picker.selected = 0;
            picker.offset = 0;
        }
        let (width, height) = size()?;
        let capacity = visible_row_capacity(height);
        let (start, end) = window(picker.selected, matching.len(), capacity, picker.offset);
        picker.offset = start;
        let help = if picker.allow_favorite {
            DEVICE_HELP
        } else {
            HELP
        };
        picker.draw(
            &mut stdout,
            Frame {
                matching: &matching,
                start,
                end,
                width,
                height,
                help,
                notice: None,
            },
        )?;

        match read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => {
                    if let Some(&index) = matching.get(picker.selected) {
                        return Ok(DevicePick::Selected(index));
                    }
                }
                KeyCode::Esc => return Ok(DevicePick::Cancelled),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(DevicePick::Cancelled);
                }
                KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
                KeyCode::Down => {
                    picker.selected = picker
                        .selected
                        .saturating_add(1)
                        .min(matching.len().saturating_sub(1));
                }
                KeyCode::PageUp => {
                    picker.selected = picker.selected.saturating_sub(capacity);
                }
                KeyCode::PageDown => {
                    picker.selected = picker
                        .selected
                        .saturating_add(capacity)
                        .min(matching.len().saturating_sub(1));
                }
                KeyCode::Home => picker.selected = 0,
                KeyCode::End => picker.selected = matching.len().saturating_sub(1),
                KeyCode::Backspace => {
                    picker.filter.pop();
                    picker.reset_viewport();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    picker.filter.clear();
                    picker.reset_viewport();
                }
                KeyCode::Char('F')
                    if picker.allow_favorite
                        && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT) =>
                {
                    if let Some(&index) = matching.get(picker.selected) {
                        return Ok(DevicePick::ToggleFavorite(index));
                    }
                }
                KeyCode::Char(input)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    picker.filter.push(input);
                    picker.reset_viewport();
                }
                _ => {}
            },
            _ => {}
        }
    }
}

struct Picker {
    title: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    filter: String,
    selected: usize,
    offset: usize,
    favorite: Option<usize>,
    allow_favorite: bool,
}

struct Frame<'a> {
    matching: &'a [usize],
    start: usize,
    end: usize,
    width: u16,
    height: u16,
    help: &'a str,
    notice: Option<&'a str>,
}

impl Picker {
    fn new(
        title: &str,
        headers: &[&str],
        rows: Vec<Vec<String>>,
        favorite: Option<usize>,
        initial_selection: usize,
        allow_favorite: bool,
    ) -> Self {
        let selected = initial_selection.min(rows.len().saturating_sub(1));
        Self {
            title: title.to_owned(),
            headers: headers.iter().map(|header| (*header).to_owned()).collect(),
            rows,
            filter: String::new(),
            selected,
            offset: 0,
            favorite,
            allow_favorite,
        }
    }

    /// Indices into `rows` whose concatenated cells match the current filter.
    fn filtered(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches(&self.filter, row))
            .map(|(index, _)| index)
            .collect()
    }

    fn reset_viewport(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    /// Redraw one frame in the alternate screen. Rows are always one terminal
    /// line high, so the table never grows beyond the available viewport.
    fn draw(&self, stdout: &mut impl Write, frame: Frame<'_>) -> Result<()> {
        let Frame {
            matching,
            start,
            end,
            width,
            height,
            help,
            notice,
        } = frame;
        let display_width = width.saturating_sub(1).max(10);
        let title = spread(&self.title, notice.unwrap_or(""), display_width as usize);
        queue!(
            stdout,
            cursor::MoveTo(0, 0),
            Clear(ClearType::All),
            Print(Style::new().bold().apply_to(title)),
        )?;

        let position = if matching.is_empty() {
            "0/0".to_owned()
        } else {
            format!("{}/{}", self.selected + 1, matching.len())
        };
        let status = format!("{position} · {} total", self.rows.len());
        let filter_line = spread(
            &format!("Filter: {}", self.filter),
            &status,
            display_width as usize,
        );
        queue!(stdout, cursor::MoveTo(0, 1), Print(filter_line),)?;

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_content_arrangement(ContentArrangement::DynamicFullWidth)
            .set_truncation_indicator("…")
            .set_width(display_width);
        let header: Vec<Cell> = self
            .headers
            .iter()
            .map(|header| {
                Cell::new(header.as_str())
                    .fg(Color::Cyan)
                    .add_attribute(Attribute::Bold)
            })
            .collect();
        let mut header = Row::from(header);
        header.max_height(1);
        table.set_header(header);
        for (position, &index) in matching[start..end].iter().enumerate() {
            let is_selected = start + position == self.selected;
            let cells: Vec<Cell> = self.rows[index]
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    let is_favorite = self.favorite == Some(index);
                    let mut cell = Cell::new(if column == 0 {
                        let cursor = if is_selected { "❯" } else { " " };
                        if self.allow_favorite {
                            let star = if is_favorite { "★" } else { " " };
                            format!("{cursor} {star} {cell}")
                        } else {
                            format!("{cursor} {cell}")
                        }
                    } else {
                        cell.clone()
                    });
                    if is_selected {
                        cell = cell.fg(Color::Cyan).add_attribute(Attribute::Bold);
                    } else if is_favorite && column == 0 {
                        cell = cell.fg(Color::Yellow).add_attribute(Attribute::Bold);
                    }
                    cell
                })
                .collect();
            let mut row = Row::from(cells);
            row.max_height(1);
            table.add_row(row);
        }

        let visible_rows = end.saturating_sub(start);
        let capacity = visible_row_capacity(height);
        if matching.is_empty() && capacity > 0 {
            let mut cells = vec![Cell::new(""); self.headers.len()];
            if let Some(first) = cells.first_mut() {
                *first = Cell::new("No matching results").fg(Color::DarkGrey);
            }
            let mut row = Row::from(cells);
            row.max_height(1);
            table.add_row(row);
        }
        let occupied = visible_rows + usize::from(matching.is_empty() && capacity > 0);
        for _ in occupied..capacity {
            let mut row = Row::from(vec![Cell::new(""); self.headers.len()]);
            row.max_height(1);
            table.add_row(row);
        }

        let table_height = height.saturating_sub(3) as usize;
        for (line, rendered) in table.lines().take(table_height).enumerate() {
            queue!(stdout, cursor::MoveTo(0, line as u16 + 2), Print(rendered))?;
        }

        if height > 0 {
            let help = truncate(help, display_width as usize);
            queue!(
                stdout,
                cursor::MoveTo(0, height - 1),
                Clear(ClearType::CurrentLine),
                Print(Style::new().dim().apply_to(help)),
            )?;
        }
        stdout.flush()?;
        Ok(())
    }
}

fn copy_to_clipboard(stdout: &mut impl Write, value: &str) -> Result<()> {
    let encoded = BASE64.encode(value.as_bytes());
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()?;
    Ok(())
}

/// Restores the previous screen even on early returns or terminal errors.
struct TtyGuard;

impl TtyGuard {
    fn enable() -> Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(std::io::stdout(), EnterAlternateScreen, cursor::Hide) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self)
    }
}

impl Drop for TtyGuard {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            cursor::Show,
            LeaveAlternateScreen,
            cursor::MoveToColumn(0)
        );
        let _ = disable_raw_mode();
    }
}

fn visible_row_capacity(height: u16) -> usize {
    // Two status lines, a four-line table frame, and one help line.
    height.saturating_sub(7).max(1) as usize
}

fn spread(left: &str, right: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let right = truncate(right, width);
    let right_width = right.chars().count();
    let left_width = width.saturating_sub(right_width + 1);
    let left = truncate(left, left_width);
    let padding = width.saturating_sub(left.chars().count() + right_width);
    format!("{left}{}{right}", " ".repeat(padding))
}

fn truncate(value: &str, width: usize) -> String {
    let length = value.chars().count();
    if length <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let mut shortened = value.chars().take(width - 1).collect::<String>();
    shortened.push('…');
    shortened
}

/// Case-insensitive substring match over the concatenated cells.
/// An empty or whitespace-only filter matches everything.
fn matches(filter: &str, row: &[String]) -> bool {
    let needle = filter.trim().to_lowercase();
    needle.is_empty() || row.concat().to_lowercase().contains(&needle)
}

/// Visible slice `[start, end)`, preserving the scroll position until the
/// selection moves beyond either edge of the viewport.
fn window(selected: usize, len: usize, capacity: usize, current_start: usize) -> (usize, usize) {
    let capacity = capacity.max(1);
    if len <= capacity {
        return (0, len);
    }

    let mut start = current_start.min(len - capacity);
    if selected < start {
        start = selected;
    } else if selected >= start + capacity {
        start = selected + 1 - capacity;
    }
    (start, (start + capacity).min(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_matches_everything() {
        let row = vec!["Foo".to_owned(), "Bar".to_owned()];
        assert!(matches("", &row));
        assert!(matches("   ", &row));
    }

    #[test]
    fn filter_is_case_insensitive_across_cells() {
        let row = vec!["Device A".to_owned(), "gateway-1".to_owned()];
        assert!(matches("device", &row));
        assert!(matches("E A", &row));
        assert!(matches("way-1", &row));
        assert!(!matches("nope", &row));
    }

    #[test]
    fn filter_spans_cell_boundary_on_concat() {
        let row = vec!["foo".to_owned(), "bar".to_owned()];
        assert!(matches("obar", &row));
    }

    #[test]
    fn small_lists_fit_one_window() {
        assert_eq!(window(2, 3, 5, 0), (0, 3));
        assert_eq!(window(0, 0, 5, 0), (0, 0));
    }

    #[test]
    fn window_scrolls_only_when_selection_crosses_an_edge() {
        assert_eq!(window(0, 100, 10, 0), (0, 10));
        assert_eq!(window(9, 100, 10, 0), (0, 10));
        assert_eq!(window(10, 100, 10, 0), (1, 11));
        assert_eq!(window(50, 100, 10, 45), (45, 55));
        assert_eq!(window(44, 100, 10, 45), (44, 54));
        assert_eq!(window(99, 100, 10, 90), (90, 100));
    }

    #[test]
    fn viewport_capacity_reserves_space_for_chrome_and_table_borders() {
        assert_eq!(visible_row_capacity(24), 17);
        assert_eq!(visible_row_capacity(8), 1);
        assert_eq!(visible_row_capacity(3), 1);
    }

    #[test]
    fn status_text_fits_the_terminal_width() {
        assert_eq!(spread("Filter: gateway", "4/120", 24).chars().count(), 24);
        assert_eq!(spread("very long filter", "1/2", 8), "ver… 1/2");
        assert_eq!(truncate("abcdefgh", 5), "abcd…");
    }

    #[test]
    fn clipboard_uses_an_osc_52_sequence_with_the_complete_value() {
        let mut output = Vec::new();
        copy_to_clipboard(&mut output, "hello world").unwrap();
        assert_eq!(output, b"\x1b]52;c;aGVsbG8gd29ybGQ=\x07");
    }
}
