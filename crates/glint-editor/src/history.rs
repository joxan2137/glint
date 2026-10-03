//! Undo/redo over whole-document snapshots. A gesture (drag, typing session, slider drag) opens with the state
//! before it and commits once at the end, so continuous edits become one step.

use crate::model::Document;

const LIMIT: usize = 200;

#[derive(Default)]
pub struct History {
    undo: Vec<Document>,
    redo: Vec<Document>,
    pending: Option<Document>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `before` as one step right away (discrete edits: delete, toggles, crop Done). While a gesture is
    /// open (typing, a slider drag) the edit joins that gesture's step instead, so the stack stays in order.
    pub fn record(&mut self, before: &Document) {
        if self.pending.is_none() {
            self.push(before.clone());
        }
    }

    /// True while a gesture's step is open.
    pub fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    /// Starts a gesture; nested calls keep the first snapshot.
    pub fn begin(&mut self, before: &Document) {
        if self.pending.is_none() {
            self.pending = Some(before.clone());
        }
    }

    /// Ends the gesture: one undo step if the document changed, nothing otherwise. Returns whether it changed.
    pub fn commit(&mut self, current: &Document) -> bool {
        match self.pending.take() {
            Some(before) if before != *current => {
                self.push(before);
                true
            }
            _ => false,
        }
    }

    /// Abandons the gesture and returns the state before it.
    pub fn cancel(&mut self) -> Option<Document> {
        self.pending.take()
    }

    fn push(&mut self, before: Document) {
        if self.undo.last() == Some(&before) {
            return;
        }
        self.undo.push(before);
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self, current: &Document) -> Option<Document> {
        if let Some(before) = self.pending.take()
            && before != *current
        {
            self.redo.push(current.clone());
            return Some(before);
        }
        let previous = self.undo.pop()?;
        self.redo.push(current.clone());
        Some(previous)
    }

    pub fn redo(&mut self, current: &Document) -> Option<Document> {
        self.pending = None;
        let next = self.redo.pop()?;
        self.undo.push(current.clone());
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use glint_core::{PointF, RectF, ToneMapParams};

    use super::*;
    use crate::model::{Body, Redaction, RedactKind};

    fn doc_with(n: usize) -> Document {
        let mut doc = Document::new(ToneMapParams::default());
        for i in 0..n {
            doc.add(Body::Redact(Redaction { rect: RectF::new(i as f32, 0.0, 1.0, 1.0), kind: RedactKind::Blur }));
        }
        doc
    }

    #[test]
    fn drag_coalesces_into_one_step() {
        let mut history = History::new();
        let mut doc = doc_with(1);
        let start = doc.clone();
        history.begin(&doc);
        for _ in 0..30 {
            doc.annotations[0].translate(PointF::new(1.0, 0.0));
            history.begin(&doc);
        }
        assert!(history.commit(&doc));
        let restored = history.undo(&doc).unwrap();
        assert_eq!(restored, start);
        assert!(!history.can_undo());
        assert_eq!(history.redo(&restored).unwrap(), doc);
    }

    #[test]
    fn unchanged_gesture_leaves_no_step() {
        let mut history = History::new();
        let doc = doc_with(2);
        history.begin(&doc);
        assert!(!history.commit(&doc));
        assert!(!history.can_undo());
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut history = History::new();
        let a = doc_with(1);
        let b = doc_with(2);
        history.record(&a);
        let back = history.undo(&b).unwrap();
        assert_eq!(back, a);
        assert!(history.can_redo());
        history.record(&back);
        assert!(!history.can_redo());
    }

    #[test]
    fn undo_mid_gesture_restores_the_gesture_start() {
        let mut history = History::new();
        let start = doc_with(1);
        history.begin(&start);
        let mut doc = start.clone();
        doc.annotations.clear();
        assert_eq!(history.undo(&doc).unwrap(), start);
        assert!(history.cancel().is_none(), "the gesture is over");
    }

    #[test]
    fn discrete_edits_join_an_open_gesture() {
        let mut history = History::new();
        let start = doc_with(1);
        history.begin(&start);
        let mut doc = start.clone();
        doc.annotations[0].translate(PointF::new(3.0, 0.0));
        history.record(&doc);
        doc.annotations.clear();
        assert!(history.commit(&doc));
        assert_eq!(history.undo(&doc).unwrap(), start, "one step back to before the gesture");
        assert!(!history.can_undo());
    }

    #[test]
    fn depth_is_bounded() {
        let mut history = History::new();
        for i in 0..(LIMIT + 50) {
            history.record(&doc_with(i % 2));
        }
        assert!(history.undo.len() <= LIMIT);
    }
}
