//! BiDi isolation of rendered TUI regions for VTE-based terminals.
//!
//! VTE runs the Unicode BiDi algorithm (implicit mode) over each terminal
//! row. Neutral characters between two RTL runs — spaces and the `│` box
//! borders — resolve to the surrounding RTL level (UAX #9 rule N1), merging
//! the runs of adjacent panes into one reversed sequence, so panes appear to
//! exchange sides. Wrapping every region's rendered line in a left-to-right
//! isolate (LRI … PDI) keeps each region an independent LTR layout unit while
//! the terminal still performs RTL ordering and Arabic shaping inside the
//! isolate.
//!
//! The controls are zero-width format characters appended to existing cells:
//! VTE stores zero-width characters on the preceding cell and feeds all of a
//! cell's codepoints to FriBidi, while Ratatui geometry, styles, and mouse
//! coordinates are unaffected. Requires FriBidi >= 1.0 for isolate support
//! (satisfied by every distribution shipping a BiDi-capable VTE).

use ratatui::{
    buffer::{Buffer, Cell, CellDiffOption, CellWidth},
    layout::Rect,
};

/// U+2066 LEFT-TO-RIGHT ISOLATE: opens a left-to-right isolate.
pub const LRI: char = '\u{2066}';
/// U+2069 POP DIRECTIONAL ISOLATE: closes the innermost open isolate.
pub const PDI: char = '\u{2069}';

/// Wrap every horizontal interval of `regions` visible in `buf` in LRI … PDI.
///
/// `regions` are the rectangles the components actually rendered into,
/// collected in back-to-front paint order. Overlapping regions are partitioned
/// by the topmost owner at each horizontal interval, and adjacent intervals
/// with the same owner are merged. Occluded pane boundaries therefore cannot
/// split a popup's contiguous text into separate isolates.
///
/// * LRI is appended to the cell immediately left of the interval (typically
///   the region's left border), or to its first printable cell when it starts
///   at the left buffer edge. Left-edge regions must therefore begin with a
///   visible neutral border or padding cell; emitting a bare zero-width
///   control at column zero would make VTE discard it.
/// * PDI is appended to the last printable cell of the interval.
///
/// "Printable" means a cell the backend actually emits. Ratatui's diff skips
/// the trailing columns covered by multi-width symbols, so those columns
/// cannot host a control even though their buffer symbol is a space.
pub fn isolate_regions(buf: &mut Buffer, regions: &[Rect]) {
    let area = buf.area;
    let clipped: Vec<(u16, u16, u16, u16)> = regions
        .iter()
        .filter_map(|region| {
            let left = region.left().max(area.left());
            let right = region.right().min(area.right());
            let top = region.top().max(area.top());
            let bottom = region.bottom().min(area.bottom());
            (left < right && top < bottom).then_some((left, right, top, bottom))
        })
        .collect();
    let Some(first) = clipped.first() else {
        return;
    };
    let top = clipped
        .iter()
        .fold(first.2, |top, region| top.min(region.2));
    let bottom = clipped
        .iter()
        .fold(first.3, |bottom, region| bottom.max(region.3));

    // Reuse row scratch storage. Rendering already visits the whole frame;
    // this pass adds no per-row allocation and skips rows outside all regions.
    let mut bounds: Vec<u16> = Vec::with_capacity(clipped.len() * 2);
    let mut segments: Vec<(u16, u16, usize)> = Vec::with_capacity(clipped.len() * 2);
    let mut hostable = Vec::with_capacity(area.width as usize);

    for y in top..bottom {
        bounds.clear();
        segments.clear();
        for &(left, right, region_top, region_bottom) in &clipped {
            if y >= region_top && y < region_bottom {
                bounds.push(left);
                bounds.push(right);
            }
        }
        if bounds.is_empty() {
            continue;
        }
        bounds.sort_unstable();
        bounds.dedup();
        for window in bounds.windows(2) {
            let (start, end) = (window[0], window[1]);
            let owner = clipped
                .iter()
                .rposition(|&(left, right, region_top, region_bottom)| {
                    y >= region_top && y < region_bottom && left <= start && end <= right
                });
            let Some(owner) = owner else {
                continue;
            };
            match segments.last_mut() {
                Some((_, previous_end, previous_owner))
                    if *previous_end == start && *previous_owner == owner =>
                {
                    *previous_end = end;
                }
                _ => segments.push((start, end, owner)),
            }
        }
        fill_row_hostable(buf, y, &mut hostable);
        for &(start, end, _) in &segments {
            wrap_segment(buf, y, &hostable, start, end);
        }
    }
}

/// Append one balanced LRI … PDI pair around the row segment `[start, end)`.
fn wrap_segment(buf: &mut Buffer, y: u16, hostable: &[bool], start: u16, end: u16) {
    let area_x = buf.area.x;
    let is_hostable = |x: u16| hostable[(x - area_x) as usize];
    let pdi_x = (start..end).rev().find(|&x| is_hostable(x));
    let Some(pdi_x) = pdi_x else {
        // Nothing printed in this interval; keep controls balanced by
        // emitting none.
        return;
    };
    let lri_x = if start > area_x {
        // Prefer the closest printable cell to the left. If `start - 1` is a
        // wide-character continuation this finds its base cell, keeping the
        // region's first character inside the isolate.
        (area_x..start)
            .rev()
            .find(|&x| is_hostable(x))
            .or_else(|| (start..=pdi_x).find(|&x| is_hostable(x)))
            .unwrap_or(pdi_x)
    } else {
        // At column zero VTE drops a bare zero-width control, so the first
        // printable cell must be the region's neutral border or padding host.
        (start..=pdi_x).find(|&x| is_hostable(x)).unwrap_or(pdi_x)
    };
    append_symbol(buf, lri_x, y, LRI);
    append_symbol(buf, pdi_x, y, PDI);
}

/// Fill the cells that Ratatui's diff iterator will emit for this row.
///
/// `Buffer::set_stringn` resets the trailing cells of a wide grapheme to
/// ordinary spaces, so `Cell::skip` alone cannot identify them. Track the
/// columns covered by each cell using Ratatui's own `CellWidth` calculation.
fn fill_row_hostable(buf: &Buffer, y: u16, hostable: &mut Vec<bool>) {
    hostable.clear();
    let mut covered_until = buf.area.left();
    for x in buf.area.left()..buf.area.right() {
        let cell = &buf[(x, y)];
        hostable.push(x >= covered_until && !cell_is_skipped(cell) && !cell.symbol().is_empty());
        covered_until = covered_until.max(x.saturating_add(cell.cell_width().max(1)));
    }
}
/// Mirror Ratatui's compatibility logic for both the current diff option and
/// the deprecated `skip` field still used by older widgets.
#[allow(deprecated)]
fn cell_is_skipped(cell: &Cell) -> bool {
    matches!(cell.diff_option, CellDiffOption::Skip)
        || (cell.skip && matches!(cell.diff_option, CellDiffOption::None))
}

/// Append a zero-width control character to a cell's symbol, preserving its
/// base character and style.
fn append_symbol(buf: &mut Buffer, x: u16, y: u16, control: char) {
    let cell = &mut buf[(x, y)];
    let mut symbol = cell.symbol().to_string();
    symbol.push(control);
    cell.set_symbol(&symbol);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};

    /// Count (LRI, PDI) occurrences across every cell of the buffer.
    fn control_counts(buf: &Buffer) -> (usize, usize) {
        let mut counts = (0, 0);
        for y in buf.area.top()..buf.area.bottom() {
            for x in buf.area.left()..buf.area.right() {
                for c in buf[(x, y)].symbol().chars() {
                    if c == LRI {
                        counts.0 += 1;
                    } else if c == PDI {
                        counts.1 += 1;
                    }
                }
            }
        }
        counts
    }

    /// Every cell must keep its base character as a prefix, gain only
    /// zero-width controls, and retain its style.
    fn assert_geometry_and_styles_preserved(before: &Buffer, after: &Buffer) {
        assert_eq!(before.area, after.area);
        for y in before.area.top()..before.area.bottom() {
            for x in before.area.left()..before.area.right() {
                let (before_cell, after_cell) = (&before[(x, y)], &after[(x, y)]);
                let base = before_cell.symbol();
                assert!(
                    after_cell.symbol().starts_with(base),
                    "cell ({x}, {y}): {:?} lost its base character {:?}",
                    after_cell.symbol(),
                    base
                );
                assert!(
                    after_cell.symbol()[base.len()..]
                        .chars()
                        .all(|c| c == LRI || c == PDI),
                    "cell ({x}, {y}): unexpected suffix in {:?}",
                    after_cell.symbol()
                );
                assert_eq!(after_cell.fg, before_cell.fg, "cell ({x}, {y}) fg");
                assert_eq!(after_cell.bg, before_cell.bg, "cell ({x}, {y}) bg");
                assert_eq!(
                    after_cell.modifier, before_cell.modifier,
                    "cell ({x}, {y}) modifier"
                );
            }
        }
    }

    #[test]
    fn two_adjacent_arabic_regions_are_isolated_separately() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 11, 1));
        buf.set_string(0, 0, "│سلام│دنیا│", Style::default());
        let before = buf.clone();

        isolate_regions(&mut buf, &[Rect::new(0, 0, 5, 1), Rect::new(5, 0, 6, 1)]);

        // First region starts at the buffer edge: LRI follows its border.
        assert_eq!(buf[(0, 0)].symbol(), "│\u{2066}");
        // Shared boundary cell hosts the first region's PDI followed by the
        // second region's LRI, in that logical order.
        assert_eq!(buf[(4, 0)].symbol(), "م\u{2069}\u{2066}");
        // Second region's PDI closes after its last printed cell.
        assert_eq!(buf[(10, 0)].symbol(), "│\u{2069}");
        assert_eq!(control_counts(&buf), (2, 2));
        assert_geometry_and_styles_preserved(&before, &buf);
    }

    #[test]
    fn nonzero_buffer_origin_uses_left_border_as_isolate_host() {
        let mut buf = Buffer::empty(Rect::new(5, 7, 11, 2));
        buf.set_string(5, 7, "│سلام│دنیا│", Style::default());
        let before = buf.clone();

        isolate_regions(&mut buf, &[Rect::new(5, 7, 5, 1), Rect::new(10, 7, 6, 1)]);

        assert_eq!(buf[(5, 7)].symbol(), "│\u{2066}");
        assert_eq!(buf[(9, 7)].symbol(), "م\u{2069}\u{2066}");
        assert_eq!(buf[(15, 7)].symbol(), "│\u{2069}");
        assert_eq!(control_counts(&buf), (2, 2));
        for x in 5..16 {
            assert_eq!(buf[(x, 8)].symbol(), before[(x, 8)].symbol());
        }
        assert_geometry_and_styles_preserved(&before, &buf);
    }

    #[test]
    fn mixed_content_and_emoji_keep_geometry() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        buf.set_string(2, 0, "Hi سلام 123 👋!", Style::default());
        let before = buf.clone();

        isolate_regions(&mut buf, &[Rect::new(2, 0, 15, 1)]);

        // LRI attaches to the cell left of the region.
        assert_eq!(buf[(1, 0)].symbol(), " \u{2066}");
        // PDI attaches to the last printed cell, after the wide emoji.
        assert_eq!(buf[(16, 0)].symbol(), "!\u{2069}");
        // Ratatui resets the emoji's hidden continuation column to an
        // ordinary space; the isolation pass must still leave it untouched.
        assert!(!cell_is_skipped(&buf[(15, 0)]));
        assert_eq!(buf[(15, 0)].symbol(), " ");
        assert_eq!(control_counts(&buf), (1, 1));
        assert_geometry_and_styles_preserved(&before, &buf);
    }

    #[test]
    fn styled_spans_keep_their_styles() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
        let styled = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        buf.set_string(1, 0, "سلام", styled);
        let before = buf.clone();

        isolate_regions(&mut buf, &[Rect::new(1, 0, 4, 1)]);

        assert_eq!(buf[(0, 0)].symbol(), " \u{2066}");
        assert_eq!(buf[(4, 0)].symbol(), "م\u{2069}");
        assert_eq!(control_counts(&buf), (1, 1));
        assert_geometry_and_styles_preserved(&before, &buf);
    }

    #[test]
    fn right_aligned_text_is_wrapped_around_its_padding() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        buf.set_string(5, 0, "سلام", Style::default());
        let before = buf.clone();

        isolate_regions(&mut buf, &[Rect::new(0, 0, 10, 1)]);

        // Region touches the left buffer edge: LRI follows the first cell.
        assert_eq!(buf[(0, 0)].symbol(), " \u{2066}");
        // PDI follows the last printed cell of the region (trailing space).
        assert_eq!(buf[(9, 0)].symbol(), " \u{2069}");
        assert_eq!(control_counts(&buf), (1, 1));
        assert_geometry_and_styles_preserved(&before, &buf);
    }

    #[test]
    fn popup_occludes_pane_boundary_without_splitting_its_isolate() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        for y in 0..3 {
            buf.set_string(0, y, "aaaaaaaaaaaaaaabbbbbbbbbbbbbbb", Style::default());
        }
        buf.set_string(10, 1, "POPUP-POP!", Style::default());
        let before = buf.clone();

        let panes = [Rect::new(0, 0, 15, 3), Rect::new(15, 0, 15, 3)];
        let popup = Rect::new(10, 1, 10, 1);
        isolate_regions(&mut buf, &[panes[0], panes[1], popup]);

        // Rows without the popup: one isolate pair per pane.
        for y in [0, 2] {
            let mut row_counts = (0, 0);
            for x in 0..30 {
                row_counts.0 += buf[(x, y)].symbol().matches(LRI).count();
                row_counts.1 += buf[(x, y)].symbol().matches(PDI).count();
            }
            assert_eq!(row_counts, (2, 2), "row {y}");
        }
        // Popup row: the topmost popup owns [10, 20) as one contiguous
        // segment; the invisible pane boundary at x=15 must not split it.
        let mut row_counts = (0, 0);
        for x in 0..30 {
            row_counts.0 += buf[(x, 1)].symbol().matches(LRI).count();
            row_counts.1 += buf[(x, 1)].symbol().matches(PDI).count();
        }
        assert_eq!(row_counts, (3, 3));
        assert!(buf[(9, 1)].symbol().contains(PDI));
        assert!(buf[(19, 1)].symbol().contains(PDI));
        assert!(!buf[(14, 1)].symbol().contains(LRI));
        assert!(!buf[(14, 1)].symbol().contains(PDI));
        assert_geometry_and_styles_preserved(&before, &buf);
    }

    #[test]
    fn wide_character_at_segment_end_hosts_pdi_on_its_base_cell() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
        buf.set_string(1, 0, "ab界", Style::default());
        let before = buf.clone();

        // Region ends exactly after the wide character: Ratatui's diff skips
        // its reset continuation column even though `Cell::skip` is false.
        isolate_regions(&mut buf, &[Rect::new(1, 0, 4, 1)]);

        assert_eq!(buf[(0, 0)].symbol(), " \u{2066}");
        assert_eq!(buf[(3, 0)].symbol(), "界\u{2069}");
        assert!(!cell_is_skipped(&buf[(4, 0)]));
        assert_eq!(buf[(4, 0)].symbol(), " ");
        assert_eq!(control_counts(&buf), (1, 1));
        // Ratatui's final buffer diff emits the base cell carrying PDI and
        // skips only the wide character's hidden continuation column.
        let updates = before.diff(&buf);
        assert!(updates
            .iter()
            .any(|(x, y, cell)| *x == 3 && *y == 0 && cell.symbol() == "界\u{2069}"));
        assert!(!updates.iter().any(|(x, y, _)| *x == 4 && *y == 0));
        assert_geometry_and_styles_preserved(&before, &buf);
    }

    #[test]
    fn wide_left_neighbor_hosts_lri_on_its_base_cell() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        buf.set_string(0, 0, "界", Style::default());
        buf.set_string(2, 0, "│سلام│", Style::default());
        let before = buf.clone();

        // Cell 1 is hidden by 界. The LRI must attach to cell 0 rather than
        // opening after the region's first printable cell at x=2.
        isolate_regions(&mut buf, &[Rect::new(2, 0, 6, 1)]);

        assert_eq!(buf[(0, 0)].symbol(), "界\u{2066}");
        assert_eq!(buf[(1, 0)].symbol(), " ");
        assert_eq!(buf[(7, 0)].symbol(), "│\u{2069}");
        assert_eq!(control_counts(&buf), (1, 1));
        assert_geometry_and_styles_preserved(&before, &buf);
    }

    #[test]
    fn controls_are_balanced_on_every_row_and_untouched_rows_stay_clean() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 4));
        buf.set_string(0, 0, "│سلام│", Style::default());
        buf.set_string(0, 1, "│دنیا│", Style::default());
        let before = buf.clone();

        isolate_regions(&mut buf, &[Rect::new(0, 0, 6, 2)]);

        assert_eq!(control_counts(&buf), (2, 2));
        // Rows outside every region are left alone.
        for y in 2..4 {
            for x in 0..20 {
                assert_eq!(buf[(x, y)].symbol(), before[(x, y)].symbol());
            }
        }
        assert_geometry_and_styles_preserved(&before, &buf);
    }
}
