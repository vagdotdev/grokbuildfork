//! The welcome hero: a torus ("donut") drawn in ASCII, spinning.
//!
//! The classic terminal donut, re-derived: a circle of radius [`TUBE`] centred [`RING`] from the
//! origin is swept around the Y axis to make the torus; every surface point is rotated about X by
//! `a` and about Z by `b`, projected through a pinhole camera [`CAMERA`] units away, depth-tested
//! per cell and lit by one light above and behind the viewer. The luminance picks a glyph from
//! [`RAMP`]; the pager maps ramp bands onto theme colours.
//!
//! The hero is tiny (14 x 7 cells), so a frame is rendered [`SUPERSAMPLE`]x finer than the cell
//! grid and each cell takes the mean luminance of its lit sub-samples: the edge stays smooth and
//! the hole stays visible. A cell mostly missed by the surface stays blank.
//!
//! Frames are computed once each, the first time they are shown, and kept for the process's
//! lifetime ([`frame`]); [`FRAMES`] of them make one seamless loop.

use std::sync::OnceLock;

/// Luminance ramp, darkest first: the glyph at index `n` shows luminance band `n`.
pub const RAMP: [char; 12] = ['.', ',', '-', '~', ':', ';', '=', '!', '*', '#', '$', '@'];

/// Frames in one loop: `a` makes two turns and `b` one, so the frame after the last is the first.
pub const FRAMES: usize = 144;

/// Angles of frame 0: a three-quarter view with the hole showing, also the static fallback.
const A0: f32 = 1.0;
const B0: f32 = 0.4;

/// Radius of the tube and distance from the origin to the tube's centre line.
const TUBE: f32 = 1.0;
const RING: f32 = 2.0;
/// Distance from the viewer to the torus's centre. Far enough that the near side is barely
/// larger than the far side, so the mark does not bob inside its slot as it turns.
const CAMERA: f32 = 10.0;
/// Fraction of the grid's half width the torus's outer radius reaches at depth 0.
const FILL: f32 = 0.9375;
/// Sub-samples per cell along each axis.
const SUPERSAMPLE: usize = 4;
/// Share of a cell's sub-samples the surface must cover for the cell to show a glyph.
const COVERAGE: f32 = 0.35;
/// Surface sampling steps: around the tube and around the ring.
const TUBE_STEP: f32 = 0.07;
const RING_STEP: f32 = 0.02;
/// Terminal cells are about twice as tall as wide.
const CELL_ASPECT: f32 = 0.5;
/// Light direction (toward the light), left unnormalised as in the classic so the full ramp is used.
const LIGHT: [f32; 3] = [0.0, 1.0, -1.0];

/// The two grids the hero is drawn at: the upstream full and small logo grids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// 7 rows x 14 columns, the hero box's mark.
    Full,
    /// 5 rows x 10 columns, for short terminals and the minimal welcome card.
    Compact,
}

impl Size {
    pub const fn cols(self) -> usize {
        match self {
            Self::Full => 14,
            Self::Compact => 10,
        }
    }

    pub const fn rows(self) -> usize {
        match self {
            Self::Full => 7,
            Self::Compact => 5,
        }
    }
}

/// One rendered frame: a luminance band per cell, row-major, `None` where the background shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    size: Size,
    cells: Vec<Option<u8>>,
}

impl Frame {
    pub fn size(&self) -> Size {
        self.size
    }

    /// Luminance band (an index into [`RAMP`]) of the cell, `None` when blank or off the grid.
    pub fn level(&self, row: usize, col: usize) -> Option<u8> {
        if col >= self.size.cols() {
            return None;
        }
        self.cells
            .get(row * self.size.cols() + col)
            .copied()
            .flatten()
    }

    /// The cell's glyph, a space when blank.
    pub fn glyph(&self, row: usize, col: usize) -> char {
        self.level(row, col)
            .and_then(|l| RAMP.get(usize::from(l)).copied())
            .unwrap_or(' ')
    }

    /// The frame as text, one line per row, blanks as spaces.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity((self.size.cols() + 1) * self.size.rows());
        for row in 0..self.size.rows() {
            for col in 0..self.size.cols() {
                out.push(self.glyph(row, col));
            }
            out.push('\n');
        }
        out
    }

    /// Cells that show a glyph.
    pub fn lit_cells(&self) -> usize {
        self.cells.iter().filter(|c| c.is_some()).count()
    }
}

/// The loop's angles for a frame index.
fn angles(index: usize) -> (f32, f32) {
    let t = (index % FRAMES) as f32 / FRAMES as f32;
    (
        A0 + 2.0 * std::f32::consts::TAU * t,
        B0 + std::f32::consts::TAU * t,
    )
}

/// Frame `index` of the loop (taken modulo [`FRAMES`]) at `size`, rendered on first use.
pub fn frame(size: Size, index: usize) -> &'static Frame {
    static FULL: [OnceLock<Frame>; FRAMES] = [const { OnceLock::new() }; FRAMES];
    static COMPACT: [OnceLock<Frame>; FRAMES] = [const { OnceLock::new() }; FRAMES];
    let index = index % FRAMES;
    let slots = match size {
        Size::Full => &FULL,
        Size::Compact => &COMPACT,
    };
    let slot = slots.get(index).unwrap_or_else(|| &slots[0]);
    slot.get_or_init(|| {
        let (a, b) = angles(index);
        render(size, a, b)
    })
}

/// Render the torus rotated by `a` about X and `b` about Z into a `size` grid.
pub fn render(size: Size, a: f32, b: f32) -> Frame {
    let (cols, rows) = (size.cols(), size.rows());
    let (w, h) = (cols * SUPERSAMPLE, rows * SUPERSAMPLE);
    // Nearest depth (as 1/z) and the luminance of the surface point that reached it, per sub-sample.
    let mut depth = vec![0.0f32; w * h];
    let mut lum: Vec<Option<f32>> = vec![None; w * h];
    let focal = (w as f32 / 2.0) * FILL * CAMERA / (RING + TUBE);
    let (sin_a, cos_a, sin_b, cos_b) = (a.sin(), a.cos(), b.sin(), b.cos());
    let rotate = |x: f32, y: f32, z: f32| -> (f32, f32, f32) {
        // About X by `a`, then about Z by `b`.
        let (y1, z1) = (y * cos_a - z * sin_a, y * sin_a + z * cos_a);
        (x * cos_b - y1 * sin_b, x * sin_b + y1 * cos_b, z1)
    };

    let mut theta = 0.0f32;
    while theta < std::f32::consts::TAU {
        let (sin_t, cos_t) = theta.sin_cos();
        let radius = RING + TUBE * cos_t;
        let mut phi = 0.0f32;
        while phi < std::f32::consts::TAU {
            let (sin_p, cos_p) = phi.sin_cos();
            // A point on the tube circle (in the XY plane, centred at x = RING) swept around Y by phi;
            // its normal is the same construction without the radii.
            let (x, y, z) = rotate(radius * cos_p, TUBE * sin_t, -radius * sin_p);
            let (nx, ny, nz) = rotate(cos_t * cos_p, sin_t, -cos_t * sin_p);
            let inv_z = 1.0 / (CAMERA + z);
            let px = (w as f32 / 2.0 + focal * inv_z * x).floor();
            let py = (h as f32 / 2.0 - focal * inv_z * y * CELL_ASPECT).floor();
            if px >= 0.0 && px < w as f32 && py >= 0.0 && py < h as f32 {
                let idx = px as usize + w * py as usize;
                if let (Some(d), Some(l)) = (depth.get_mut(idx), lum.get_mut(idx))
                    && inv_z > *d
                {
                    *d = inv_z;
                    *l = Some(nx * LIGHT[0] + ny * LIGHT[1] + nz * LIGHT[2]);
                }
            }
            phi += RING_STEP;
        }
        theta += TUBE_STEP;
    }

    let mut cells = Vec::with_capacity(cols * rows);
    let per_cell = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    for row in 0..rows {
        for col in 0..cols {
            let (mut hit, mut total) = (0usize, 0.0f32);
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let idx = col * SUPERSAMPLE + sx + w * (row * SUPERSAMPLE + sy);
                    if let Some(Some(l)) = lum.get(idx) {
                        hit += 1;
                        total += l.max(0.0);
                    }
                }
            }
            let covered = hit as f32 / per_cell >= COVERAGE;
            cells.push(covered.then(|| {
                // Mean luminance over the lit sub-samples, in [0, sqrt 2), spread over the ramp.
                let mean = total / hit.max(1) as f32;
                ((mean * 8.0) as u8).min(RAMP.len() as u8 - 1)
            }));
        }
    }
    Frame { size, cells }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame 0 of the full-size hero, pinned: the static fallback and the first frame of the loop.
    const FRAME_0_FULL: &str = concat!(
        "   $$$$$$##   \n",
        " ***!!=;;==!= \n",
        ";==:~,....,~::\n",
        "::~,.   :,,---\n",
        ",---~!#$*=:-, \n",
        " .,-~;==;~-.  \n",
        "     ..       \n",
    );

    #[test]
    fn frame_zero_matches_the_snapshot() {
        assert_eq!(frame(Size::Full, 0).text(), FRAME_0_FULL);
    }

    #[test]
    fn frames_fill_the_hero_grids() {
        for size in [Size::Full, Size::Compact] {
            let (cols, rows) = (size.cols(), size.rows());
            let mut rows_used = std::collections::BTreeSet::new();
            let mut cols_used = std::collections::BTreeSet::new();
            for i in 0..FRAMES {
                let f = frame(size, i);
                assert_eq!(f.size(), size);
                assert_eq!(f.text().lines().count(), rows, "{size:?} frame {i}");
                assert!(
                    f.text().lines().all(|l| l.chars().count() == cols),
                    "{size:?} frame {i} is {cols} wide"
                );
                let lit = f.lit_cells();
                let area = cols * rows;
                assert!(
                    lit * 4 >= area && lit * 10 <= area * 9,
                    "{size:?} frame {i}: {lit} of {area} cells lit"
                );
                for row in 0..rows {
                    for col in 0..cols {
                        if let Some(level) = f.level(row, col) {
                            assert!(usize::from(level) < RAMP.len());
                            rows_used.insert(row);
                            cols_used.insert(col);
                        }
                    }
                }
            }
            // Over a loop the torus visits every row and column: the mark uses its whole slot.
            assert_eq!(rows_used.len(), rows, "{size:?} rows {rows_used:?}");
            assert_eq!(cols_used.len(), cols, "{size:?} cols {cols_used:?}");
        }
    }

    #[test]
    fn the_donut_turns_and_the_loop_closes() {
        let first = frame(Size::Full, 0);
        assert_ne!(first, frame(Size::Full, 1), "consecutive frames differ");
        assert!(
            (0..FRAMES).any(|i| frame(Size::Full, i).level(3, 6).is_none()),
            "some frame shows the hole in the middle"
        );
        // Rendering one loop further lands on the same picture.
        let (a, b) = angles(0);
        let wrapped = render(
            Size::Full,
            a + 2.0 * std::f32::consts::TAU,
            b + std::f32::consts::TAU,
        );
        let diff = (0..Size::Full.rows())
            .flat_map(|r| (0..Size::Full.cols()).map(move |c| (r, c)))
            .filter(|&(r, c)| wrapped.level(r, c) != first.level(r, c))
            .count();
        assert!(diff <= 2, "{diff} cells differ after a full loop");
        assert!(std::ptr::eq(frame(Size::Full, FRAMES), first));
    }

    #[test]
    fn frames_are_rendered_once() {
        assert!(std::ptr::eq(
            frame(Size::Compact, 5),
            frame(Size::Compact, 5)
        ));
        assert!(!std::ptr::eq(
            frame(Size::Compact, 5),
            frame(Size::Compact, 6)
        ));
    }

    #[test]
    fn blank_cells_and_the_edge_of_the_grid_read_as_spaces() {
        let f = frame(Size::Full, 0);
        assert_eq!(f.glyph(6, 0), ' ');
        assert_eq!(f.level(0, 14), None, "past the last column");
        assert_eq!(f.level(7, 0), None, "past the last row");
        assert_eq!(f.glyph(0, 5), '$');
        assert_eq!(f.level(0, 5), Some(10));
    }
}
