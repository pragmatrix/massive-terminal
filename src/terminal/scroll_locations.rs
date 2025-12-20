//! Because of precision problems, we need to bucket allocate transforms. This way, we can share
//! transforms, which - supposedly - can be optimized by the renderer in the future.

use std::{collections::HashMap, ops::Range};

use derive_more::{Deref, From};
use log::trace;

use wezterm_term::StableRowIndex;

use massive_scene::{Handle, Location, Transform};
use massive_shell::Scene;

use crate::range_ops::*;

/// A bucket allocator for scroll matrices. The buckets are defined by a fixed number of StableRowIndex.
#[derive(Debug)]
pub struct ScrollLocations {
    parent: Handle<Location>,
    line_height_px: u32,
    /// The current scroll offset in pixels.
    scroll_offset_px: i64,
    // Performance: Use a simple vec here.
    locations: HashMap<BucketKey, ScrollLocation>,
}

#[cfg(debug_assertions)]
const BUCKET_SIZE: usize = 16;
#[cfg(not(debug_assertions))]
const BUCKET_SIZE: usize = 1024;

#[derive(Debug)]
struct ScrollLocation {
    /// The current pixel offset for the matrix of this this location (this is the amount the matrix
    /// scrolls the content upwards).
    pub matrix_scroll_offset_px: i64,
    pub location: Handle<Location>,
}

impl ScrollLocations {
    pub fn new(parent: Handle<Location>, line_height_px: u32, scroll_offset_px: i64) -> Self {
        Self {
            parent,
            line_height_px,
            scroll_offset_px,
            locations: Default::default(),
        }
    }

    /// Acquire a new location for a line and a line top offset in pixel.
    pub fn acquire_line_location(
        &mut self,
        scene: &Scene,
        stable_index: StableRowIndex,
    ) -> (Handle<Location>, i64) {
        let bucket_key = Self::bucket_key(stable_index);

        let line_height_px = self.line_height_px as i64;
        let stable_line_offset_px = stable_index as i64 * line_height_px;
        let bucket_stable_range = Self::bucket_stable_range(bucket_key);
        // Absolute offset of the buckets first line.
        let bucket_top_px = bucket_stable_range.start as i64 * line_height_px;
        let line_offset_px = stable_line_offset_px - bucket_top_px;

        use std::collections::hash_map::Entry;

        let location = match self.locations.entry(bucket_key) {
            Entry::Occupied(occupied) => occupied.into_mut().location.clone(),
            Entry::Vacant(vacant) => {
                let matrix_scroll_offset_px = bucket_top_px - self.scroll_offset_px;
                let transform =
                    scene.stage(Transform::from((0., matrix_scroll_offset_px as f64, 0.)));
                let location = scene.stage(Location::new(Some(self.parent.clone()), transform));
                let scroll_location = ScrollLocation {
                    location: location.clone(),
                    matrix_scroll_offset_px,
                };
                vacant.insert(scroll_location);
                location
            }
        };

        (location, line_offset_px)
    }

    pub fn scroll_offset_px(&self) -> i64 {
        self.scroll_offset_px
    }

    /// Scroll the _current_ scroll offset of all matrices to the pixel location.
    pub fn set_scroll_offset_px(&mut self, scroll_offset_px: i64) {
        let line_height = self.line_height_px;
        self.locations.iter_mut().for_each(|(index, location)| {
            let base_offset = Self::bucket_base_scroll_offset(*index, line_height);
            let new_scroll_offset = base_offset - scroll_offset_px;
            if new_scroll_offset != location.matrix_scroll_offset_px {
                location.matrix_scroll_offset_px = new_scroll_offset;
                location
                    .location
                    .value()
                    .transform
                    .update(Transform::from_translation((
                        0.,
                        new_scroll_offset as f64,
                        0.,
                    )));
            }
        });

        self.scroll_offset_px = scroll_offset_px;
    }

    /// Sets the active range in use.
    ///
    /// Calling this function removes all the bucket's locations that are outside this range.
    pub fn mark_used(&mut self, stable_range: Range<StableRowIndex>) {
        let locations_before = self.locations.len();

        self.locations.retain(|bucket_index, _| {
            stable_range.intersects(&Self::bucket_stable_range(*bucket_index))
        });

        let locations_after = self.locations.len();
        if locations_before != locations_after {
            trace!(
                "Number of scroll locations reduced from {locations_before} to {locations_after}"
            )
        }
    }

    /// The bucket's base scroll offset.
    ///
    /// This added to the scroll offset in `ScrollLocation` makes up the final scroll offset.
    fn bucket_base_scroll_offset(index: BucketKey, line_height: u32) -> i64 {
        let top = Self::bucket_stable_range(index).start;
        top as i64 * line_height as i64
    }

    fn bucket_key(stable_index: StableRowIndex) -> BucketKey {
        stable_index.div_euclid(BUCKET_SIZE as isize).into()
    }

    fn bucket_stable_range(index: BucketKey) -> Range<StableRowIndex> {
        (*index * BUCKET_SIZE as isize).with_len(BUCKET_SIZE)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Deref, From)]
struct BucketKey(isize);
