//! Undo and redo by keeping earlier states of the whole project. A project is small (sample
//! audio is not part of it), so whole copies are cheap, and every edit is undoable without
//! writing an inverse for it.
//!
//! The app calls `observe` once a frame. A change becomes one undo step when it settles: a drag,
//! a scroll or typing in a field counts as one step, committed when the pointer is released,
//! the scrolling stops or the field loses focus.

/// Undo steps kept; older ones are dropped.
pub const LIMIT: usize = 200;

pub struct History<T> {
    /// The state as of the last committed step.
    base: T,
    undo: Vec<T>,
    redo: Vec<T>,
}

impl<T: Clone + PartialEq> History<T> {
    pub fn new(state: &T) -> Self {
        History { base: state.clone(), undo: Vec::new(), redo: Vec::new() }
    }

    /// Forgets everything (a new or opened project).
    pub fn reset(&mut self, state: &T) {
        *self = History::new(state);
    }

    /// Records `current` as a new step if it changed and the edit has `settled`.
    pub fn observe(&mut self, current: &T, settled: bool) {
        if settled && *current != self.base {
            self.commit(current);
        }
    }

    fn commit(&mut self, current: &T) {
        let before = std::mem::replace(&mut self.base, current.clone());
        self.undo.push(before);
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn can_undo(&self, current: &T) -> bool {
        !self.undo.is_empty() || *current != self.base
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Steps `current` back. An edit still in progress is committed first, so it is what gets
    /// undone. False if there is nothing to undo.
    pub fn undo(&mut self, current: &mut T) -> bool {
        if *current != self.base {
            self.commit(current);
        }
        let Some(before) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(current, before));
        self.base = current.clone();
        true
    }

    pub fn redo(&mut self, current: &mut T) -> bool {
        if *current != self.base {
            // Edited since the undo: that edit replaced the redo steps.
            self.commit(current);
            return false;
        }
        let Some(after) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(current, after));
        self.base = current.clone();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_back_and_forward() {
        let mut v = 0;
        let mut h = History::new(&v);
        assert!(!h.can_undo(&v) && !h.can_redo());
        for x in [1, 2, 3] {
            v = x;
            h.observe(&v, true);
        }
        assert!(h.undo(&mut v) && v == 2);
        assert!(h.undo(&mut v) && v == 1);
        assert!(h.redo(&mut v) && v == 2);
        assert!(h.undo(&mut v) && h.undo(&mut v) && v == 0);
        assert!(!h.undo(&mut v) && v == 0);
        assert!(h.redo(&mut v) && h.redo(&mut v) && h.redo(&mut v) && v == 3);
        assert!(!h.redo(&mut v));
    }

    #[test]
    fn a_drag_is_one_step() {
        let mut v = 0;
        let mut h = History::new(&v);
        for x in 1..=10 {
            v = x;
            h.observe(&v, false); // pointer still down
        }
        h.observe(&v, true);
        h.observe(&v, true); // nothing new
        assert!(h.undo(&mut v) && v == 0);
        assert!(!h.can_undo(&v));
    }

    #[test]
    fn undo_takes_back_an_edit_in_progress() {
        let mut v = 0;
        let mut h = History::new(&v);
        v = 5;
        h.observe(&v, false);
        assert!(h.can_undo(&v));
        assert!(h.undo(&mut v) && v == 0);
        assert!(h.redo(&mut v) && v == 5);
    }

    #[test]
    fn a_new_edit_drops_the_redo_steps() {
        let mut v = 0;
        let mut h = History::new(&v);
        v = 1;
        h.observe(&v, true);
        h.undo(&mut v);
        v = 7;
        h.observe(&v, true);
        assert!(!h.can_redo());
        assert!(!h.redo(&mut v) && v == 7);
        assert!(h.undo(&mut v) && v == 0);
        // Editing after an undo without observing first: redo commits the edit instead.
        h.redo(&mut v);
        v = 9;
        assert!(!h.redo(&mut v) && v == 9);
        assert!(h.undo(&mut v) && v == 7);
    }

    #[test]
    fn keeps_at_most_limit_steps() {
        let mut v = 0;
        let mut h = History::new(&v);
        for x in 1..=(LIMIT as i32 + 50) {
            v = x;
            h.observe(&v, true);
        }
        let mut n = 0;
        while h.undo(&mut v) {
            n += 1;
        }
        assert_eq!(n, LIMIT);
        assert_eq!(v, 50);
    }

    #[test]
    fn reset_forgets() {
        let mut v = 0;
        let mut h = History::new(&v);
        v = 1;
        h.observe(&v, true);
        h.reset(&v);
        assert!(!h.can_undo(&v) && !h.undo(&mut v) && v == 1);
    }
}
