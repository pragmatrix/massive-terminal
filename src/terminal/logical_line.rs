use std::ops::Range;

use wezterm_term::{Line, StableRowIndex, Terminal};

pub fn get_logical_lines(terminal: &Terminal, lines: Range<StableRowIndex>) -> Vec<LogicalLine> {
    let mut logical_lines = Vec::new();

    terminal
        .screen()
        .for_each_logical_line_in_stable_range(lines, |stable_range, lines| {
            logical_lines.push(LogicalLine::from_physical_range(stable_range, lines));
            true
        });

    logical_lines
}

// Initially copied from wezterm's mux/src/pane.rs at 6a493f88fab06a792308e0c704790390fd3c6232
// This is used for implementing copy based on a selection.
#[derive(Debug, Clone, PartialEq)]
pub struct LogicalLine {
    pub physical_lines: Vec<Line>,
    pub logical: Line,
    pub first_row: StableRowIndex,
}

impl LogicalLine {
    #[allow(unused)]
    pub fn contains_y(&self, y: StableRowIndex) -> bool {
        y >= self.first_row && y < self.first_row + self.physical_lines.len() as StableRowIndex
    }

    #[allow(unused)]
    pub fn xy_to_logical_x(&self, x: usize, y: StableRowIndex) -> usize {
        let mut offset = 0;
        for (idx, line) in self.physical_lines.iter().enumerate() {
            let phys_y = self.first_row + idx as StableRowIndex;
            if y < phys_y {
                // Eg: trying to drag off the top of the viewport.
                // Their y coordinate precedes our first line, so
                // the only logical x we can return is 0
                return 0;
            }
            if phys_y == y {
                return offset + x;
            }
            offset += line.len();
        }
        // Allow selecting off the end of the line
        offset + x
    }

    #[allow(unused)]
    pub fn logical_x_to_physical_coord(&self, x: usize) -> (StableRowIndex, usize) {
        let mut y = self.first_row;
        let mut idx = 0;
        for line in &self.physical_lines {
            let x_off = x - idx;
            let line_len = line.len();
            if x_off < line_len {
                return (y, x_off);
            }
            y += 1;
            idx += line_len;
        }
        (y - 1, x - idx + self.physical_lines.last().unwrap().len())
    }
}

impl LogicalLine {
    /// Create a logical line by concatenating a number of physical lines that make up one logical line.
    pub fn from_physical_range(stable_range: Range<StableRowIndex>, lines: &[&Line]) -> Self {
        Self {
            physical_lines: lines.iter().copied().cloned().collect(),
            logical: logical_from_physicals(lines),
            first_row: stable_range.start,
        }
    }
}

fn logical_from_physicals(physical_lines: &[&Line]) -> Line {
    debug_assert!(!physical_lines.is_empty());

    let seqno = physical_lines
        .iter()
        .map(|l| l.current_seqno())
        .max()
        .unwrap();

    let mut logical_line = Line::new(seqno);

    for physical_line in physical_lines {
        if !logical_line.is_empty() {
            logical_line.set_last_cell_was_wrapped(false, seqno);
        }
        logical_line.append_line((*physical_line).clone(), seqno);
    }

    debug_assert_eq!(logical_line.current_seqno(), seqno);

    logical_line
}
