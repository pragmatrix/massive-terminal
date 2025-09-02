//! A list of scroll matrices the lines are using.
//!
//! The use of multiple scroll matrices is needed, because of numerical inaccurracies when it comes
//! to multiplying large f32 vertex coordinates with a scroll matrix, that also contains large f32
//! values. The visible inaccuracies begin about when one million lines are scrolled down. Glyph
//! boundary edges are off by one or more pixel and it gets worse.
//!
//! This is a clumsy solution for that. Maintain multiple scroll matrices and line locations, one
//! for every n lines, and remove the ones that are not used.
//!
//! While this optimization currently isn't better performing like specifying one matrix per line, I
//! prefer to use as less matrices as possible to allow for batch optimizations later in the massive
//! renderer.

use std::{collections::VecDeque, ops::Range};

use massive_scene::{Handle, Location, Matrix, Scene};
use wezterm_term::StableRowIndex;

/// The number of lines a single matrix covers.
const LINES_PER_MATRIX: usize = 0x10;

#[derive(Debug)]
pub struct ScrollMatrices {
    parent_location: Handle<Location>,

    /// Height of one single line (also cell height).
    line_height_px: u64,

    /// The top scroll offset. This is the line that is currently at the top of the viewport.
    scroll_offset: StableRowIndex,

    /// The top index of the first matrix in the deque.
    /// index 0: The first matrix covers lines at stable index 0..`LINES_PER_MATRIX`.
    /// index 1: The first matrix covers lines at stable index `LINES_PER_MATRIX`..`LINES_PER_MATRIX`*2.
    top_index: usize,

    scroll_locations: VecDeque<ScrollLocation>,
}

impl ScrollMatrices {
    pub fn new(parent_location: Handle<Location>, line_height_px: u64) -> Self {
        Self {
            parent_location,
            line_height_px,
            scroll_offset: 0,
            top_index: 0,
            scroll_locations: VecDeque::default(),
        }
    }

    /// Returns a location for the line and the top position of this line.
    // Architecture: Allow scene to be shared in the massive project.
    pub fn location_for_line(
        &mut self,
        index: StableRowIndex,
        scene: &Scene,
    ) -> (Handle<Location>, u64) {
        assert!(index >= 0);
        while index < self.coverage_stable_range().start {
            assert!(self.top_index > 0);
            let matrix_top_line_index =
                self.coverage_stable_range().start - LINES_PER_MATRIX as isize - self.scroll_offset;

            let matrix_delta_y = matrix_top_line_index as i64 * self.line_height_px as i64;

            let matrix = scene.stage(Matrix::from_translation(
                (0., matrix_delta_y as f64, 0.).into(),
            ));
            let location = scene.stage(Location::new(
                Some(self.parent_location.clone()),
                matrix.clone(),
            ));
            let scroll_location = ScrollLocation::new(matrix_delta_y, matrix, location);

            self.scroll_locations.push_front(scroll_location);
            self.top_index -= 1;
        }

        while index >= self.coverage_stable_range().end {
            let matrix_top_line_index = self.coverage_stable_range().end - self.scroll_offset;

            let matrix_delta_y = matrix_top_line_index as i64 * self.line_height_px as i64;
            let matrix = scene.stage(Matrix::from_translation(
                (0., matrix_delta_y as f64, 0.).into(),
            ));
            let location = scene.stage(Location::new(
                Some(self.parent_location.clone()),
                matrix.clone(),
            ));
            let scroll_location = ScrollLocation::new(matrix_delta_y, matrix, location);

            self.scroll_locations.push_back(scroll_location);
        }

        debug_assert!(self.coverage_stable_range().contains(&index));

        let coverage_index = (index - self.coverage_stable_range().start) as usize;
        let location_index = coverage_index / LINES_PER_MATRIX;
        let mut line_top = (coverage_index % LINES_PER_MATRIX) as u64;
        let location = &self.scroll_locations[location_index];
        line_top += location.scrolled as u64;
        let location = location.location.clone();
        (location, line_top * self.line_height_px)
    }

    /// Scroll the top position of the view to the new StableRowIndex.
    pub fn scroll_to(&mut self, new_scroll_offset: StableRowIndex) {
        let scroll_delta = new_scroll_offset - self.scroll_offset;
        let delta_y = scroll_delta as i64 * self.line_height_px as i64;
        self.scroll_locations
            .iter_mut()
            .for_each(|l| l.scroll(scroll_delta, delta_y));
        self.scroll_offset = new_scroll_offset;
    }

    // Limit the current scroll range to the stable row range. The range must cover all lines that
    // are currently visible.
    //
    // This will purge all locations hat fall outside of this range.
    pub fn limit_coverage(&mut self, range: Range<StableRowIndex>) {
        assert!(range.start >= 0 && range.end >= range.start);

        let current = self.coverage_stable_range();
        // Disjoint range: requested range lies completely outside current coverage.
        if range.end <= current.start || range.start >= current.end {
            self.top_index = (range.start as usize) / LINES_PER_MATRIX;
            self.scroll_locations.clear();
            return;
        }

        let mut trim_top = 0;
        let mut trim_bottom = 0;
        if current.start < range.start {
            trim_top = (range.start - current.start) as usize / LINES_PER_MATRIX;
        }
        if current.end > range.end {
            trim_bottom = (current.end - range.end) as usize / LINES_PER_MATRIX;
        }

        let end = self.scroll_locations.len();
        self.scroll_locations.drain(end - trim_bottom..end);
        self.scroll_locations.drain(0..trim_top);
        self.top_index += trim_top;
    }

    /// The range of rows the current set of locations cover.
    fn coverage_stable_range(&self) -> Range<StableRowIndex> {
        let start = self.top_index * LINES_PER_MATRIX;
        let end = start + self.scroll_locations.len() * LINES_PER_MATRIX;
        Range {
            start: start as isize,
            end: end as isize,
        }
    }
}

#[derive(Debug)]
struct ScrollLocation {
    /// The actual y integer translation of the matrix. This is to preserve the numerical accuracy
    /// in relation to the f64 translation when we scroll the matrix.
    y_translation: i64,
    scrolled: i64,
    matrix: Handle<Matrix>,
    location: Handle<Location>,
}

impl ScrollLocation {
    // Architecture: Location already contains the matrix, can't we access it from there when scrolling?
    fn new(y_translation: i64, matrix: Handle<Matrix>, location: Handle<Location>) -> Self {
        Self {
            scrolled: 0,
            y_translation,
            matrix,
            location,
        }
    }

    fn scroll(&mut self, lines: isize, delta_y: i64) {
        self.scrolled += lines as i64;
        self.y_translation += delta_y;
        self.matrix.update(Matrix::from_translation(
            (0., self.y_translation as f64, 0.).into(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use massive_scene::{Location, Matrix, Scene};

    // This test documents current panic behavior when a completely disjoint range is passed to
    // limit_coverage. A disjoint range causes trim_top to exceed the number of existing matrices
    // and thus panic in the drain call. If disjoint ranges should be supported, limit_coverage
    // must be adjusted. For now we lock this in with a should_panic test.
    #[test]
    fn disjoint_range_clears() {
        let scene = Scene::new();
        let parent_matrix = scene.stage(Matrix::from_translation((0., 0., 0.).into()));
        let parent_location = scene.stage(Location::from(parent_matrix));
        let mut mats = ScrollMatrices::new(parent_location, 10);

        // Populate 4 matrices (indices 0..64)
        for i in [0, 15, 16, 31, 32, 47, 48, 63] {
            // span across multiple blocks
            let _ = mats.location_for_line(i as isize, &scene);
        }

        let current = mats.coverage_stable_range();
        assert_eq!(current.start, 0);
        assert_eq!(current.end, 64);

        // Choose a disjoint range that starts far beyond current.end so that
        // trim_top = (range.start - current.start)/LINES_PER_MATRIX > number_of_matrices (4),
        // triggering the panic.
        let far_start = 1024; // well beyond 64
        mats.limit_coverage(far_start..(far_start + 1));
        assert_eq!(mats.scroll_locations.len(), 0);
        assert_eq!(mats.top_index, far_start as usize / LINES_PER_MATRIX);
        assert_eq!(mats.coverage_stable_range().start, far_start);
    }
}
