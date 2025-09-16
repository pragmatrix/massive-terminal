#![allow(unused)]
use std::{fmt::Debug, ops::Range};

use num::Integer;
use rangeset::{intersects_range, range_intersection, range_subtract, range_union};

pub trait RangeTools: Sized {
    fn intersects(&self, other: &Self) -> bool;
    fn is_inside(&self, other: &Self) -> bool;
    fn intersect(&self, other: &Self) -> Option<Self>;
    fn subtract(&self, other: &Self) -> (Option<Self>, Option<Self>);
    fn union(self, other: Self) -> Self;
}

impl<T: Integer + Copy + Debug> RangeTools for Range<T> {
    fn intersects(&self, other: &Self) -> bool {
        intersects_range(self, other)
    }

    fn is_inside(&self, other: &Self) -> bool {
        other.start <= self.start && other.end >= self.end
    }

    fn intersect(&self, other: &Self) -> Option<Self> {
        range_intersection(self, other)
    }

    fn subtract(&self, other: &Self) -> (Option<Self>, Option<Self>) {
        range_subtract(self, other)
    }

    fn union(self, other: Self) -> Self {
        range_union(self, other)
    }
}

pub trait WithLength: Sized {
    fn with_len<Len>(self, length: Len) -> Range<Self>
    where
        Self: TryFrom<Len>,
        Self::Error: Debug;
}

impl<T: Integer + Copy + Debug> WithLength for T {
    fn with_len<Len>(self, length: Len) -> Range<Self>
    where
        T: TryFrom<Len>,
        T::Error: Debug,
    {
        self..self + T::try_from(length).expect("Failed to convert length to range element type")
    }
}
