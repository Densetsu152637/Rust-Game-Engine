//! Per-tick work budget shared by the scheduler and by hosts that apply loaded cells.

/// A budget of cells and bytes for one tick.
///
/// The first item of a tick is always admitted when at least one cell is left, even if it alone
/// exceeds the byte budget, so a single large cell cannot starve forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickBudget {
    cells: usize,
    bytes: u64,
    used_cells: usize,
    used_bytes: u64,
}

impl TickBudget {
    /// A fresh budget; `bytes == u64::MAX` means unlimited bytes.
    pub fn new(cells: usize, bytes: u64) -> Self {
        Self {
            cells,
            bytes,
            used_cells: 0,
            used_bytes: 0,
        }
    }
    /// Would an item of `bytes` fit?
    pub fn can_take(&self, bytes: u64) -> bool {
        self.used_cells < self.cells
            && (self.used_cells == 0 || self.used_bytes.saturating_add(bytes) <= self.bytes)
    }
    /// Takes `bytes` if the item fits; reports whether it was admitted.
    pub fn try_take(&mut self, bytes: u64) -> bool {
        if !self.can_take(bytes) {
            return false;
        }
        self.used_cells += 1;
        self.used_bytes = self.used_bytes.saturating_add(bytes);
        true
    }
    pub fn used_cells(&self) -> usize {
        self.used_cells
    }
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_budget_limits_admissions() {
        let mut b = TickBudget::new(2, u64::MAX);
        assert!(b.try_take(10) && b.try_take(10));
        assert!(!b.try_take(1));
        assert_eq!((b.used_cells(), b.used_bytes()), (2, 20));
    }

    #[test]
    fn byte_budget_admits_the_first_item_then_stops() {
        let mut b = TickBudget::new(10, 100);
        assert!(b.try_take(250), "first oversized item is admitted");
        assert!(!b.try_take(1), "nothing else fits");
        let mut c = TickBudget::new(10, 100);
        assert!(c.try_take(60) && c.try_take(40));
        assert!(!c.try_take(1));
    }

    #[test]
    fn zero_cells_admit_nothing() {
        assert!(!TickBudget::new(0, 100).try_take(0));
    }
}
