//! The starter zero-shot label set (ML-SPEC.md §4: "a curated starter list
//! (a few dozen to ~100 common photo-content labels), zero-shot-scored
//! automatically, freely user-extensible").
//!
//! **Draft, not yet owner-confirmed** — unlike the rest of Milestone ML-2,
//! this is pure editorial content ML-SPEC.md deliberately left open
//! ("a curated starter list" with no list attached to any closed ticket).
//! Flagged distinctly from the confirmed I/O/scoring code around it.

/// One label per line, lowercase, single word/short phrase (matches how a
/// manually-typed tag already looks in this app — §4: a model-generated
/// "beach" and a hand-typed "beach" are the same tag). No hierarchy, no
/// synonyms folded together — each string here becomes its own row in
/// `tags` the first time it's scored above the storage floor.
pub const STARTER_LABELS: &[&str] = &[
    // People
    "portrait",
    "selfie",
    "group photo",
    "crowd",
    "baby",
    "child",
    "wedding",
    "graduation",
    "birthday party",
    // Animals
    "dog",
    "cat",
    "bird",
    "horse",
    "farm animal",
    "wildlife",
    "insect",
    "fish",
    // Nature & landscape
    "landscape",
    "mountain",
    "forest",
    "beach",
    "ocean",
    "lake",
    "river",
    "waterfall",
    "desert",
    "field",
    "sky",
    "clouds",
    "sunset",
    "sunrise",
    "night sky",
    "stars",
    "snow",
    "rain",
    "storm",
    "flower",
    "plant",
    "tree",
    // Urban & architecture
    "city",
    "street",
    "building",
    "architecture",
    "bridge",
    "monument",
    "interior",
    "office",
    "kitchen",
    // Travel & activity
    "travel",
    "hiking",
    "camping",
    "beach vacation",
    "road trip",
    "airport",
    "boat",
    "airplane",
    // Food & drink
    "food",
    "restaurant meal",
    "dessert",
    "coffee",
    "drink",
    // Vehicles
    "car",
    "motorcycle",
    "bicycle",
    "train",
    // Events & documents
    "concert",
    "sports",
    "fireworks",
    "holiday",
    "screenshot",
    "document",
    "whiteboard",
    // Style / composition
    "black and white",
    "macro close-up",
    "night photography",
    "aerial view",
    "underwater",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_unique_lowercase_and_nonempty() {
        let mut seen = std::collections::HashSet::new();
        for label in STARTER_LABELS {
            assert!(!label.is_empty());
            assert_eq!(*label, label.to_lowercase(), "{label:?} should already be lowercase");
            assert!(seen.insert(*label), "duplicate label: {label:?}");
        }
    }

    #[test]
    fn label_count_is_within_the_spec_a_few_dozen_to_100_range() {
        assert!(STARTER_LABELS.len() >= 24, "fewer than a few dozen labels");
        assert!(STARTER_LABELS.len() <= 100, "over §4's ~100 label ceiling");
    }
}
