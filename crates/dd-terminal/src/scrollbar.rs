//! One-column scrollbar, ported from rho's `tui/scrollbar.rs`
//! (`HistoryScrollbar`): the same thumb geometry, the same thumb/track drag
//! split, and the same `█` thumb on a dim `│` track.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

const THUMB_GLYPH: &str = "█";
const TRACK_GLYPH: &str = "│";
const THUMB_STYLE: Style = Style::new().fg(Color::Gray);
/// Brighter thumb while it is being dragged, like rho's brand-colored drag.
const THUMB_DRAG_STYLE: Style = Style::new().fg(Color::White);
const TRACK_STYLE: Style = Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM);

/// A scrollbar painted in the rightmost column of the area it scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scrollbar {
    pub(crate) rect: Rect,
    content_len: usize,
    viewport_len: usize,
    top_line: usize,
}

/// How a press on the scrollbar started: grabbing the thumb keeps the grab
/// point under the pointer; pressing the bare track centers the thumb there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollbarDrag {
    Thumb {
        thumb_grab_offset: usize,
        start_row: usize,
        start_top_line: usize,
    },
    Track {
        thumb_grab_offset: usize,
    },
}

/// Thumb rows within the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Thumb {
    top: usize,
    height: usize,
}

impl Thumb {
    fn contains(self, row: usize) -> bool {
        (self.top..self.top + self.height).contains(&row)
    }
}

/// Whether `content_len` rows overflow a `viewport_len` row view enough to
/// warrant a bar: a one-row view has no room for a usable track.
pub(crate) fn should_show(content_len: usize, viewport_len: usize) -> bool {
    viewport_len > 1 && content_len > viewport_len
}

impl Scrollbar {
    /// A bar for `area` showing `content_len` rows from `top_line`, or `None`
    /// when the content fits.
    pub(crate) fn new(area: Rect, content_len: usize, top_line: usize) -> Option<Self> {
        let viewport_len = area.height as usize;
        if area.width == 0 || !should_show(content_len, viewport_len) {
            return None;
        }
        Some(Self {
            rect: Rect::new(
                area.x.saturating_add(area.width.saturating_sub(1)),
                area.y,
                1,
                area.height,
            ),
            content_len,
            viewport_len,
            top_line,
        })
    }

    pub(crate) fn contains(&self, column: u16, row: u16) -> bool {
        self.rect.contains((column, row).into())
    }

    pub(crate) fn begin_drag(&self, row: u16) -> ScrollbarDrag {
        let row = self.clamped_track_row(row);
        let thumb = self.thumb();
        if thumb.contains(row) {
            ScrollbarDrag::Thumb {
                thumb_grab_offset: row.saturating_sub(thumb.top),
                start_row: row,
                start_top_line: self.top_line.min(self.max_top_line()),
            }
        } else {
            ScrollbarDrag::Track {
                thumb_grab_offset: thumb.height / 2,
            }
        }
    }

    /// The first visible content row for the pointer at `row` during `drag`.
    pub(crate) fn top_line_for_pointer(&self, row: u16, drag: ScrollbarDrag) -> usize {
        let row = self.clamped_track_row(row);
        let thumb_grab_offset = match drag {
            ScrollbarDrag::Thumb {
                thumb_grab_offset,
                start_row,
                start_top_line,
            } => {
                if row == start_row {
                    return start_top_line;
                }
                thumb_grab_offset
            }
            ScrollbarDrag::Track { thumb_grab_offset } => thumb_grab_offset,
        };
        let thumb = self.thumb();
        let max_thumb_top = (self.rect.height as usize).saturating_sub(thumb.height);
        if max_thumb_top == 0 {
            return 0;
        }
        let thumb_top = row.saturating_sub(thumb_grab_offset).min(max_thumb_top);
        rounding_divide(thumb_top.saturating_mul(self.max_top_line()), max_thumb_top)
    }

    pub(crate) fn render(&self, buffer: &mut Buffer, dragging: bool) {
        let thumb = self.thumb();
        let thumb_style = if dragging {
            THUMB_DRAG_STYLE
        } else {
            THUMB_STYLE
        };
        for row in 0..self.rect.height {
            let (symbol, style) = if thumb.contains(row as usize) {
                (THUMB_GLYPH, thumb_style)
            } else {
                (TRACK_GLYPH, TRACK_STYLE)
            };
            // Reset first: `set_style` patches, and the text row's own style
            // (the selection inversion) must not leak into the bar.
            buffer[(self.rect.x, self.rect.y.saturating_add(row))]
                .set_symbol(symbol)
                .set_style(Style::reset().patch(style));
        }
    }

    fn thumb(&self) -> Thumb {
        let track_height = self.rect.height as usize;
        let height = rounding_divide(
            self.viewport_len.saturating_mul(track_height),
            self.content_len,
        )
        .clamp(1, track_height.max(1));
        let max_thumb_top = track_height.saturating_sub(height);
        let max_top_line = self.max_top_line();
        let top = if max_thumb_top == 0 || max_top_line == 0 {
            0
        } else {
            rounding_divide(
                self.top_line
                    .min(max_top_line)
                    .saturating_mul(max_thumb_top),
                max_top_line,
            )
            .min(max_thumb_top)
        };
        Thumb { top, height }
    }

    fn clamped_track_row(&self, row: u16) -> usize {
        if row <= self.rect.y {
            0
        } else {
            row.saturating_sub(self.rect.y)
                .min(self.rect.height.saturating_sub(1)) as usize
        }
    }

    fn max_top_line(&self) -> usize {
        self.content_len.saturating_sub(self.viewport_len)
    }
}

fn rounding_divide(numerator: usize, denominator: usize) -> usize {
    (numerator + denominator / 2)
        .checked_div(denominator)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(content_len: usize, top_line: usize) -> Scrollbar {
        Scrollbar::new(Rect::new(0, 10, 20, 4), content_len, top_line).expect("overflows")
    }

    #[test]
    fn no_bar_when_content_fits_or_view_is_one_row() {
        assert_eq!(Scrollbar::new(Rect::new(0, 0, 20, 4), 4, 0), None);
        assert_eq!(Scrollbar::new(Rect::new(0, 0, 20, 1), 9, 0), None);
    }

    #[test]
    fn bar_takes_the_rightmost_column() {
        assert_eq!(bar(8, 0).rect, Rect::new(19, 10, 1, 4));
        assert!(bar(8, 0).contains(19, 13));
        assert!(!bar(8, 0).contains(18, 13));
    }

    #[test]
    fn thumb_scales_with_the_view_and_follows_the_top_line() {
        // 4 of 8 rows visible: a half-track thumb, at the top then bottom.
        assert_eq!(bar(8, 0).thumb(), Thumb { top: 0, height: 2 });
        assert_eq!(bar(8, 4).thumb(), Thumb { top: 2, height: 2 });
    }

    #[test]
    fn thumb_drag_keeps_the_grab_point() {
        let scrollbar = bar(8, 0);
        let drag = scrollbar.begin_drag(11);
        assert!(matches!(drag, ScrollbarDrag::Thumb { .. }));
        // Not moved yet: the view stays put.
        assert_eq!(scrollbar.top_line_for_pointer(11, drag), 0);
        // Thumb dragged to the track bottom: last screenful.
        assert_eq!(scrollbar.top_line_for_pointer(13, drag), 4);
        assert_eq!(scrollbar.top_line_for_pointer(99, drag), 4);
    }

    #[test]
    fn track_press_centers_the_thumb_on_the_pointer() {
        let scrollbar = bar(8, 0);
        let drag = scrollbar.begin_drag(13);
        assert!(matches!(drag, ScrollbarDrag::Track { .. }));
        assert_eq!(scrollbar.top_line_for_pointer(13, drag), 4);
    }
}
