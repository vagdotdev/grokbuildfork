//! The welcome hero's loop: the donut spins, the `v` monogram comes up through it, rests, and goes
//! back down while the donut keeps turning.
//!
//! One hero loop is one donut loop ([`FRAMES`]), so the pager's single frame counter drives both
//! and hero frame `i` always shows the donut at its own angle `i`: the `v` only covers the donut
//! for a while, it never stops it. The swap is a per-cell dissolve that sweeps from the bottom row
//! up, with a fixed jitter per cell, so the `v` rises through the donut and sinks back the same way.

use std::sync::OnceLock;

use crate::donut::{self, Size};

/// Frames in one hero loop: exactly one donut loop, so the loop closes where it started.
pub const FRAMES: usize = donut::FRAMES;

/// The loop's timeline in frames (the pager ticks at about 12 fps): the donut alone until
/// `SPIN_END`, the `v` rising until `RISE_END`, resting until `REST_END`, then sinking to the end.
const SPIN_END: usize = 96;
const RISE_END: usize = SPIN_END + 12;
const REST_END: usize = RISE_END + 24;

/// The `v` monogram at the two hero grids, in braille dots; U+2800 is a blank cell.
const MARK_FULL: &str = include_str!("../assets/monogram-sans-7x14.txt");
const MARK_COMPACT: &str = include_str!("../assets/monogram-sans-5x10.txt");

/// What one hero cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Blank,
    /// A donut cell: its luminance band, an index into [`donut::RAMP`].
    Donut(u8),
    /// A cell of the `v`: its braille glyph.
    Mark(char),
}

/// One composed hero frame, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    size: Size,
    cells: Vec<Cell>,
}

impl Frame {
    pub fn size(&self) -> Size {
        self.size
    }

    /// The cell at (`row`, `col`); [`Cell::Blank`] off the grid.
    pub fn cell(&self, row: usize, col: usize) -> Cell {
        if col >= self.size.cols() {
            return Cell::Blank;
        }
        self.cells
            .get(row * self.size.cols() + col)
            .copied()
            .unwrap_or(Cell::Blank)
    }

    /// The cell's glyph, a space when blank.
    pub fn glyph(&self, row: usize, col: usize) -> char {
        match self.cell(row, col) {
            Cell::Blank => ' ',
            Cell::Donut(level) => donut::RAMP.get(usize::from(level)).copied().unwrap_or(' '),
            Cell::Mark(c) => c,
        }
    }

    /// The frame as text, one line per row, blanks as spaces.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity((self.size.cols() + 1) * self.size.rows() * 3);
        for row in 0..self.size.rows() {
            for col in 0..self.size.cols() {
                out.push(self.glyph(row, col));
            }
            out.push('\n');
        }
        out
    }
}

/// Hero frame `index` (taken modulo [`FRAMES`]) at `size`, composed on first use.
pub fn frame(size: Size, index: usize) -> &'static Frame {
    static FULL: [OnceLock<Frame>; FRAMES] = [const { OnceLock::new() }; FRAMES];
    static COMPACT: [OnceLock<Frame>; FRAMES] = [const { OnceLock::new() }; FRAMES];
    let index = index % FRAMES;
    let slots = match size {
        Size::Full => &FULL,
        Size::Compact => &COMPACT,
    };
    let slot = slots.get(index).unwrap_or_else(|| &slots[0]);
    slot.get_or_init(|| compose(size, index))
}

fn compose(size: Size, index: usize) -> Frame {
    let spin = donut::frame(size, index);
    let share = mark_share(index);
    let cells = (0..size.rows())
        .flat_map(|row| (0..size.cols()).map(move |col| (row, col)))
        .map(|(row, col)| {
            if swap_at(size, row, col) < share {
                mark(size, row, col).map_or(Cell::Blank, Cell::Mark)
            } else {
                spin.level(row, col).map_or(Cell::Blank, Cell::Donut)
            }
        })
        .collect();
    Frame { size, cells }
}

/// How much of frame `index` the `v` covers: 0 the donut alone, 1 the `v` alone.
fn mark_share(index: usize) -> f32 {
    let i = index % FRAMES;
    if i < SPIN_END {
        0.0
    } else if i < RISE_END {
        (i - SPIN_END + 1) as f32 / (RISE_END - SPIN_END) as f32
    } else if i < REST_END {
        1.0
    } else {
        1.0 - (i - REST_END + 1) as f32 / (FRAMES - REST_END) as f32
    }
}

/// The share at which a cell swaps, in [0, 1): the bottom row first and the top row last, with a
/// fixed per-cell jitter so the edge is ragged rather than a straight wipe.
fn swap_at(size: Size, row: usize, col: usize) -> f32 {
    let rows = size.rows();
    let height = rows.saturating_sub(1 + row) as f32 / rows as f32;
    let mut h = (row as u32).wrapping_mul(0x9E37_79B9) ^ (col as u32).wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    let jitter = (h >> 8) as f32 / (1u32 << 24) as f32;
    height * 0.75 + jitter * 0.25
}

/// The `v`'s glyph at (`row`, `col`), `None` where its grid is blank.
fn mark(size: Size, row: usize, col: usize) -> Option<char> {
    let art = match size {
        Size::Full => MARK_FULL,
        Size::Compact => MARK_COMPACT,
    };
    art.lines()
        .nth(row)?
        .chars()
        .nth(col)
        .filter(|c| *c != '\u{2800}' && *c != ' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark_cells(f: &Frame) -> usize {
        let size = f.size();
        (0..size.rows())
            .flat_map(|r| (0..size.cols()).map(move |c| (r, c)))
            .filter(|&(r, c)| matches!(f.cell(r, c), Cell::Mark(_)))
            .count()
    }

    #[test]
    fn the_mark_fills_the_hero_grids_and_reads_as_a_v() {
        for size in [Size::Full, Size::Compact] {
            let art = match size {
                Size::Full => MARK_FULL,
                Size::Compact => MARK_COMPACT,
            };
            let lines: Vec<&str> = art.lines().collect();
            assert_eq!(lines.len(), size.rows(), "{size:?}");
            assert!(lines.iter().all(|l| l.chars().count() == size.cols()));
            let (last, cols) = (size.rows() - 1, size.cols());
            // Arms at the top corners, one apex at the bottom centre.
            assert!(mark(size, 0, 1).is_some() && mark(size, 0, cols - 2).is_some());
            assert!(mark(size, 0, cols / 2).is_none());
            assert!(mark(size, last, 0).is_none() && mark(size, last, cols - 1).is_none());
            assert!((cols / 2 - 2..cols / 2 + 1).any(|c| mark(size, last, c).is_some()));
        }
    }

    #[test]
    fn the_loop_starts_and_ends_on_the_donut_alone() {
        for size in [Size::Full, Size::Compact] {
            for i in (0..SPIN_END).chain([FRAMES - 1]) {
                let f = frame(size, i);
                assert_eq!(f.text(), donut::frame(size, i).text(), "{size:?} frame {i}");
                assert_eq!(mark_cells(f), 0);
            }
        }
        assert!(std::ptr::eq(
            frame(Size::Full, FRAMES),
            frame(Size::Full, 0)
        ));
    }

    #[test]
    fn the_v_rises_rests_and_sinks() {
        for size in [Size::Full, Size::Compact] {
            let rest = frame(size, RISE_END);
            for row in 0..size.rows() {
                for col in 0..size.cols() {
                    let want = mark(size, row, col).map_or(Cell::Blank, Cell::Mark);
                    assert_eq!(rest.cell(row, col), want, "{size:?} rest {row},{col}");
                }
            }
            for i in RISE_END..REST_END {
                assert_eq!(frame(size, i), rest, "{size:?} the v rests at frame {i}");
            }
            let full = mark_cells(rest);
            let rising: Vec<usize> = (SPIN_END..RISE_END)
                .map(|i| mark_cells(frame(size, i)))
                .collect();
            let sinking: Vec<usize> = (REST_END..FRAMES)
                .map(|i| mark_cells(frame(size, i)))
                .collect();
            assert!(rising.windows(2).all(|w| w[0] <= w[1]), "{rising:?}");
            assert!(sinking.windows(2).all(|w| w[0] >= w[1]), "{sinking:?}");
            assert!(rising[0] < full && rising[rising.len() - 1] == full);
            assert_eq!(sinking[sinking.len() - 1], 0);
        }
    }

    #[test]
    fn the_v_comes_up_from_the_bottom() {
        let size = Size::Full;
        let last = size.rows() - 1;
        let mean = |row: usize| {
            (0..size.cols()).map(|c| swap_at(size, row, c)).sum::<f32>() / size.cols() as f32
        };
        assert!(
            (0..last).all(|r| mean(r + 1) < mean(r)),
            "lower rows swap first"
        );
        for row in 0..size.rows() {
            for col in 0..size.cols() {
                let t = swap_at(size, row, col);
                assert!((0.0..1.0).contains(&t), "{row},{col}: {t}");
            }
        }
    }

    #[test]
    fn the_donut_keeps_turning_under_the_dissolve() {
        // Wherever a dissolving frame still shows the donut, it shows the donut at that frame's
        // own angle: the spin carries on through the rise and the sink.
        for i in (SPIN_END..RISE_END).chain(REST_END..FRAMES) {
            let f = frame(Size::Full, i);
            let spin = donut::frame(Size::Full, i);
            for row in 0..Size::Full.rows() {
                for col in 0..Size::Full.cols() {
                    if let Cell::Donut(level) = f.cell(row, col) {
                        assert_eq!(spin.level(row, col), Some(level), "frame {i} {row},{col}");
                    }
                }
            }
        }
    }

    #[test]
    fn glyphs_and_the_edge_of_the_grid() {
        let rest = frame(Size::Full, RISE_END);
        assert_eq!(rest.glyph(0, 1), '\u{2880}');
        assert_eq!(rest.glyph(0, 0), ' ');
        assert_eq!(rest.cell(0, 14), Cell::Blank, "past the last column");
        assert_eq!(rest.cell(7, 0), Cell::Blank, "past the last row");
        assert_eq!(frame(Size::Full, 0).glyph(0, 5), '$');
    }
}
