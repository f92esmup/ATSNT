//! The account's book: one position per instrument, and the limits a new entry must respect
//! against it.

use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::decision::PositionView;
use crate::instrument::InstrumentId;
use crate::position::PositionSide;

/// One open position of the book and the margin it ties up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookEntry {
    /// The position.
    pub position: PositionView,
    /// Margin posted for it, in quote units (its notional on spot).
    pub margin: Decimal,
}

/// The open positions of an account, at most one per instrument, in instrument order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    positions: BTreeMap<InstrumentId, BookEntry>,
}

impl Book {
    /// An empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the position of `instrument`, replacing any earlier one.
    pub fn open(&mut self, instrument: InstrumentId, entry: BookEntry) {
        self.positions.insert(instrument, entry);
    }

    /// Removes the position of `instrument`, returning it.
    pub fn close(&mut self, instrument: &InstrumentId) -> Option<BookEntry> {
        self.positions.remove(instrument)
    }

    /// The position of `instrument`, if open.
    pub fn get(&self, instrument: &InstrumentId) -> Option<&BookEntry> {
        self.positions.get(instrument)
    }

    /// The open positions, in instrument order.
    pub fn iter(&self) -> impl Iterator<Item = (&InstrumentId, &BookEntry)> {
        self.positions.iter()
    }

    /// Number of open positions.
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Whether no position is open.
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Number of open positions on `side`.
    pub fn count_side(&self, side: PositionSide) -> usize {
        self.positions
            .values()
            .filter(|entry| entry.position.side == side)
            .count()
    }

    /// Margin the open positions tie up.
    pub fn committed_margin(&self) -> Decimal {
        self.positions.values().map(|entry| entry.margin).sum()
    }

    /// Gross notional of the open positions, each marked at `mark(instrument)` or, when it has no
    /// mark, at its entry price.
    pub fn gross_exposure(&self, mark: impl Fn(&InstrumentId) -> Option<Decimal>) -> Decimal {
        self.positions
            .iter()
            .map(|(instrument, entry)| {
                let price = mark(instrument).unwrap_or(entry.position.entry_price);
                entry.position.quantity.abs() * price
            })
            .sum()
    }
}

/// Limits on the book that a new entry must respect. The default allows one position at a time
/// with no other limit: the rule of a single-instrument account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortfolioLimits {
    /// Most positions open at once.
    pub max_open_positions: usize,
    /// Most positions open at once on one side, when limited.
    pub max_same_direction: Option<usize>,
    /// Most gross notional open at once, as a multiple of equity, when limited.
    pub max_gross_exposure_pct: Option<Decimal>,
}

impl Default for PortfolioLimits {
    fn default() -> Self {
        Self {
            max_open_positions: 1,
            max_same_direction: None,
            max_gross_exposure_pct: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn entry(side: PositionSide, quantity: Decimal, price: Decimal) -> BookEntry {
        BookEntry {
            position: PositionView {
                side,
                quantity,
                entry_price: price,
            },
            margin: quantity * price,
        }
    }

    #[test]
    fn the_book_counts_sides_margin_and_marked_exposure() {
        let btc = InstrumentId::from_static("BTCUSDT");
        let eth = InstrumentId::from_static("ETHUSDT");
        let mut book = Book::new();
        book.open(btc.clone(), entry(PositionSide::Long, dec!(1), dec!(100)));
        book.open(eth.clone(), entry(PositionSide::Short, dec!(2), dec!(10)));

        assert_eq!(book.len(), 2);
        assert_eq!(book.count_side(PositionSide::Long), 1);
        assert_eq!(book.committed_margin(), dec!(120));
        let marks = |id: &InstrumentId| (id == &btc).then_some(dec!(110));
        assert_eq!(book.gross_exposure(marks), dec!(130));
        assert_eq!(
            book.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["BTCUSDT", "ETHUSDT"]
        );

        assert!(book.close(&btc).is_some());
        assert!(book.get(&btc).is_none());
        assert_eq!(book.len(), 1);
    }
}
