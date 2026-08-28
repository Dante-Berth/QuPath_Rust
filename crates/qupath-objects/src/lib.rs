//! Columnar storage for QuPath detection objects and their measurements.
//!
//! QuPath gives every `PathObject` its own `MeasurementList`: a per-object name
//! list plus a name-to-index map, with values boxed behind a `Measurement`
//! interface. Reading one feature across a million detections therefore walks a
//! million separate allocations and hashes a million strings.
//!
//! This crate stores the same data as a struct of arrays. Feature names are
//! interned once for the whole table, and each feature is one contiguous
//! `Vec<f32>` indexed by object row. Reading a feature across all objects is a
//! linear scan of one allocation, which vectorises and prefetches; filtering
//! touches only the columns named in the predicate.
//!
//! # Precision
//!
//! Values are stored as [`f32`]. This matches QuPath: both its `FloatList` and —
//! because of an upstream truncation described in `DIVERGENCES.md` — its
//! `DoubleList` retain only `f32` precision, so `f64` columns here would double
//! memory on the largest structure in the program while storing no extra
//! information.
//!
//! # Missing values
//!
//! QuPath's `MeasurementList::get` returns `Double.NaN` for an absent
//! measurement rather than raising an error, and callers rely on that. [`Table`]
//! reproduces it: a row that never had a feature set reads back as [`f32::NAN`],
//! and [`Table::get`] on an unknown feature name returns `NaN` too.
//!
//! ```
//! use qupath_objects::Table;
//!
//! let mut table = Table::new();
//! let area = table.add_feature("Area µm^2");
//! let row = table.push_object();
//! table.set(row, area, 12.5);
//!
//! assert_eq!(table.get(row, "Area µm^2"), 12.5);
//! assert!(table.get(row, "Perimeter").is_nan()); // absent, as in QuPath
//! ```

use rustc_hash::FxHashMap;

/// Index of a feature column within a [`Table`].
///
/// Resolve a name to a `FeatureId` once, outside any loop, then index with it;
/// that is the whole point of the columnar layout. Ids are stable for the life
/// of the table — features can be added but never removed or reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FeatureId(u32);

impl FeatureId {
    /// The underlying column index.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of an object (row) within a [`Table`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(u32);

impl ObjectId {
    /// The underlying row index.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A set of detection objects and their measurements, stored column-wise.
///
/// Every column is kept exactly `n_objects` long, padded with [`f32::NAN`], so a
/// column is always directly indexable by row without a bounds negotiation.
#[derive(Debug, Clone, Default)]
pub struct Table {
    n_objects: usize,
    /// Interned feature names; position equals the [`FeatureId`] index.
    names: Vec<Box<str>>,
    /// `lookup[name]` is the id of the column called `name`.
    lookup: FxHashMap<Box<str>, FeatureId>,
    /// `columns[f][i]` is feature `f` of object `i`.
    columns: Vec<Vec<f32>>,
}

impl Table {
    /// Creates an empty table with no objects and no features.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of objects (rows).
    #[must_use]
    pub fn len(&self) -> usize {
        self.n_objects
    }

    /// Returns `true` if the table holds no objects.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.n_objects == 0
    }

    /// Number of features (columns).
    #[must_use]
    pub fn n_features(&self) -> usize {
        self.names.len()
    }

    /// Feature names, ordered by [`FeatureId`].
    #[must_use]
    pub fn feature_names(&self) -> &[Box<str>] {
        &self.names
    }

    /// Appends an object, and returns its row index.
    ///
    /// The new row reads as [`f32::NAN`] in every existing column until written.
    pub fn push_object(&mut self) -> ObjectId {
        let id = ObjectId(u32::try_from(self.n_objects).expect("row count exceeds u32"));
        self.n_objects += 1;
        for column in &mut self.columns {
            column.push(f32::NAN);
        }
        id
    }

    /// Appends `count` objects, and returns their row indices.
    ///
    /// Preferred over repeated [`Table::push_object`] when the count is known:
    /// each column is extended once rather than `count` times.
    pub fn push_objects(&mut self, count: usize) -> impl Iterator<Item = ObjectId> + use<> {
        let first = self.n_objects;
        let end = first + count;
        u32::try_from(end).expect("row count exceeds u32");
        self.n_objects = end;
        for column in &mut self.columns {
            column.resize(end, f32::NAN);
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "end fits in u32, checked above"
        )]
        ((first as u32)..(end as u32)).map(ObjectId)
    }

    /// Returns the id of the feature called `name`, adding the column if absent.
    ///
    /// A newly added column reads as [`f32::NAN`] for every existing object.
    pub fn add_feature(&mut self, name: &str) -> FeatureId {
        if let Some(&id) = self.lookup.get(name) {
            return id;
        }
        let id = FeatureId(u32::try_from(self.names.len()).expect("feature count exceeds u32"));
        let name: Box<str> = name.into();
        self.names.push(name.clone());
        self.lookup.insert(name, id);
        self.columns.push(vec![f32::NAN; self.n_objects]);
        id
    }

    /// Returns the id of the feature called `name`, or `None` if there is none.
    ///
    /// Use this once before a loop; inside the loop use [`Table::value`].
    #[must_use]
    pub fn feature(&self, name: &str) -> Option<FeatureId> {
        self.lookup.get(name).copied()
    }

    /// Writes one measurement.
    ///
    /// # Panics
    ///
    /// Panics if `object` or `feature` does not belong to this table, which
    /// indicates ids crossed between tables rather than bad input.
    pub fn set(&mut self, object: ObjectId, feature: FeatureId, value: f32) {
        self.columns[feature.index()][object.index()] = value;
    }

    /// Reads one measurement by resolved id.
    ///
    /// # Panics
    ///
    /// Panics if `object` or `feature` does not belong to this table.
    #[must_use]
    pub fn value(&self, object: ObjectId, feature: FeatureId) -> f32 {
        self.columns[feature.index()][object.index()]
    }

    /// Reads one measurement by name, returning [`f32::NAN`] if the feature does
    /// not exist.
    ///
    /// This mirrors QuPath's `MeasurementList::get`. It hashes the name on every
    /// call, so it is for one-off access and tests — never for a loop body.
    ///
    /// # Panics
    ///
    /// Panics if `object` does not belong to this table.
    #[must_use]
    pub fn get(&self, object: ObjectId, name: &str) -> f32 {
        self.feature(name)
            .map_or(f32::NAN, |feature| self.value(object, feature))
    }

    /// All values of one feature, in row order.
    ///
    /// This is the fast path: one contiguous slice, no per-object indirection.
    ///
    /// # Panics
    ///
    /// Panics if `feature` does not belong to this table.
    #[must_use]
    pub fn column(&self, feature: FeatureId) -> &[f32] {
        &self.columns[feature.index()]
    }

    /// All values of one feature, mutably.
    ///
    /// # Panics
    ///
    /// Panics if `feature` does not belong to this table.
    pub fn column_mut(&mut self, feature: FeatureId) -> &mut [f32] {
        &mut self.columns[feature.index()]
    }

    /// Row indices whose `feature` value satisfies `predicate`.
    ///
    /// Returns indices rather than cloned objects, so a filter costs one pass
    /// over one column plus the output vector.
    ///
    /// Objects whose value is [`f32::NAN`] are offered to `predicate` like any
    /// other; a predicate such as `|v| v > 10.0` therefore rejects them, which
    /// matches how QuPath's numeric predicates treat missing measurements.
    ///
    /// # Panics
    ///
    /// Panics if `feature` does not belong to this table.
    #[must_use]
    pub fn filter(&self, feature: FeatureId, predicate: impl Fn(f32) -> bool) -> Vec<u32> {
        let column = self.column(feature);
        let mut out = Vec::new();
        for (i, &v) in column.iter().enumerate() {
            if predicate(v) {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "row count is bounded by u32 at push time"
                )]
                out.push(i as u32);
            }
        }
        out
    }

    /// Releases spare capacity in every column.
    ///
    /// Worth calling once a table is fully built and will only be read.
    pub fn shrink_to_fit(&mut self) {
        self.names.shrink_to_fit();
        self.columns.shrink_to_fit();
        for column in &mut self.columns {
            column.shrink_to_fit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_measurement_reads_as_nan_like_qupath() {
        let mut table = Table::new();
        let area = table.add_feature("Area");
        let row = table.push_object();

        // Feature exists but was never written for this row.
        assert!(table.value(row, area).is_nan());
        // Feature does not exist at all.
        assert!(table.get(row, "Nucleus: Hematoxylin OD mean").is_nan());
    }

    #[test]
    fn feature_added_after_objects_is_nan_for_existing_rows() {
        let mut table = Table::new();
        let first = table.push_object();
        let late = table.add_feature("Perimeter");

        assert!(table.value(first, late).is_nan());
        assert_eq!(table.column(late).len(), 1);

        let second = table.push_object();
        table.set(second, late, 3.0);
        assert!(table.value(first, late).is_nan());
        assert_eq!(table.value(second, late), 3.0);
    }

    #[test]
    fn adding_existing_feature_returns_same_column() {
        let mut table = Table::new();
        let first = table.add_feature("Area");
        let row = table.push_object();
        table.set(row, first, 7.0);

        let again = table.add_feature("Area");

        assert_eq!(first, again);
        assert_eq!(table.n_features(), 1);
        assert_eq!(table.value(row, again), 7.0);
    }

    #[test]
    fn columns_stay_aligned_with_rows() {
        let mut table = Table::new();
        let area = table.add_feature("Area");
        let rows: Vec<_> = table.push_objects(4).collect();
        let perimeter = table.add_feature("Perimeter");

        assert_eq!(rows.len(), 4);
        assert_eq!(table.len(), 4);
        assert_eq!(table.column(area).len(), 4);
        assert_eq!(table.column(perimeter).len(), 4);
        assert_eq!(rows[3].index(), 3);
    }

    #[test]
    fn filter_returns_matching_rows_and_rejects_nan() {
        let mut table = Table::new();
        let area = table.add_feature("Area");
        let rows: Vec<_> = table.push_objects(4).collect();
        table.set(rows[0], area, 5.0);
        table.set(rows[1], area, 50.0);
        table.set(rows[2], area, 500.0);
        // rows[3] deliberately left unset, so it is NaN.

        let big = table.filter(area, |v| v > 10.0);

        assert_eq!(big, vec![1, 2]);
    }

    #[test]
    fn push_objects_matches_repeated_push_object() {
        let mut bulk = Table::new();
        let bulk_area = bulk.add_feature("Area");
        let bulk_rows: Vec<_> = bulk.push_objects(3).collect();

        let mut one_at_a_time = Table::new();
        let single_area = one_at_a_time.add_feature("Area");
        let single_rows: Vec<_> = (0..3).map(|_| one_at_a_time.push_object()).collect();

        for (i, (&b, &s)) in bulk_rows.iter().zip(single_rows.iter()).enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "small test values")]
            let v = i as f32;
            bulk.set(b, bulk_area, v);
            one_at_a_time.set(s, single_area, v);
        }

        assert_eq!(bulk.len(), one_at_a_time.len());
        assert_eq!(bulk.column(bulk_area), one_at_a_time.column(single_area));
    }
}
