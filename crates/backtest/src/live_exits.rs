//! Live exit lifecycle: exchange-held SL/TP, cleanup, time barrier and emergency close.
//!
//! The manager owns the decisions and a bounded retry policy; every exchange call goes through
//! [`ExitGateway`], so the state machine is tested without a network. Nothing here sleeps:
//! persistent failures stay visible in the state and are retried on the next tick or bar.

use domain::Side;
use rust_decimal::Decimal;

pub use ports::ExitGateway;

/// Immediate attempts per operation before the failure is reported to the caller.
const MAX_ATTEMPTS: usize = 3;

/// Emergency state of the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltState {
    /// Normal operation.
    Running,
    /// Kill-switch fired but the position is not yet confirmed flat; retried on every call.
    Halting,
    /// Position confirmed flat and orders cancelled; no new entries until restart.
    Halted,
}

/// What happened, for logging and alerting by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitEvent {
    /// Both protective orders are live on the exchange.
    Protected,
    /// Protection failed; the position was closed at market and orders cancelled.
    UnprotectedEntryClosed { reason: String },
    /// Protection failed and the emergency close also failed: the position may be naked.
    UnprotectedEntryCloseFailed { reason: String },
    /// The exchange closed the position; leftover orders were cancelled.
    CleanedUp,
    /// Leftover orders could not be cancelled yet; retried on the next bar.
    CleanupPending { reason: String },
    /// The time barrier closed the position at market.
    TimeBarrierClosed,
    /// The time barrier close failed; the exchange-held SL/TP remain and it is retried.
    TimeBarrierCloseFailed { reason: String },
    /// The kill-switch flattened the session; no new entries until restart.
    Halted,
    /// The kill-switch could not confirm a flat position; retried on the next call.
    HaltingFailed { reason: String },
}

#[derive(Debug, Clone, Copy)]
struct TrackedPosition {
    max_bars_hold: usize,
    bars_held: usize,
}

/// Owns the exit lifecycle of the single live position of one symbol.
#[derive(Debug)]
pub struct ExitManager {
    symbol: String,
    position: Option<TrackedPosition>,
    cleanup_pending: bool,
    /// Protection failed and the emergency close failed too: retried on every bar.
    unprotected: bool,
    halt: HaltState,
}

impl ExitManager {
    /// Creates an idle manager for `symbol`.
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            position: None,
            cleanup_pending: false,
            unprotected: false,
            halt: HaltState::Running,
        }
    }

    /// Current emergency state.
    #[inline]
    pub fn halt_state(&self) -> HaltState {
        self.halt
    }

    /// Whether a new entry may be dispatched: running, flat and with no leftover orders.
    #[inline]
    pub fn can_open(&self) -> bool {
        self.halt == HaltState::Running && self.position.is_none() && !self.cleanup_pending
    }

    /// Entry filled: install the exchange-held exits, or flatten if that cannot be confirmed.
    pub async fn on_entry_filled<G: ExitGateway>(
        &mut self,
        gateway: &G,
        entry_side: Side,
        stop_loss: Decimal,
        take_profit: Decimal,
        max_bars_hold: usize,
    ) -> ExitEvent {
        self.position = Some(TrackedPosition {
            max_bars_hold,
            bars_held: 0,
        });
        let mut last_error = String::new();
        for _ in 0..MAX_ATTEMPTS {
            match gateway
                .place_exits(&self.symbol, entry_side, stop_loss, Some(take_profit))
                .await
            {
                Ok(()) => return ExitEvent::Protected,
                Err(error) => {
                    last_error = error.to_string();
                    // A partial placement must not stack with the retry.
                    let _ = gateway.cancel_orders(&self.symbol).await;
                }
            }
        }
        match self.flatten(gateway).await {
            Ok(()) => {
                self.position = None;
                self.unprotected = false;
                ExitEvent::UnprotectedEntryClosed { reason: last_error }
            }
            Err(close_error) => {
                self.unprotected = true;
                ExitEvent::UnprotectedEntryCloseFailed {
                    reason: format!("{last_error}; close failed: {close_error}"),
                }
            }
        }
    }

    /// The exchange closed the position (SL, TP or manual): cancel what is left.
    pub async fn on_position_closed_by_exchange<G: ExitGateway>(
        &mut self,
        gateway: &G,
    ) -> ExitEvent {
        self.position = None;
        self.cleanup_pending = true;
        self.try_cleanup(gateway).await
    }

    /// A bar closed: retry pending cleanup and enforce the time barrier.
    pub async fn on_bar_closed<G: ExitGateway>(&mut self, gateway: &G) -> Option<ExitEvent> {
        if self.unprotected {
            // A naked position is retried every bar, not only at the time barrier.
            return Some(match self.flatten(gateway).await {
                Ok(()) => {
                    self.position = None;
                    self.unprotected = false;
                    ExitEvent::UnprotectedEntryClosed {
                        reason: "retried close succeeded".to_string(),
                    }
                }
                Err(reason) => ExitEvent::UnprotectedEntryCloseFailed { reason },
            });
        }
        if self.cleanup_pending {
            return Some(self.try_cleanup(gateway).await);
        }
        let tracked = self.position.as_mut()?;
        tracked.bars_held += 1;
        if tracked.bars_held < tracked.max_bars_hold {
            return None;
        }
        // Close first and cancel afterwards: the exchange-held exits protect until the
        // position is really flat.
        match self.flatten(gateway).await {
            Ok(()) => {
                self.position = None;
                Some(ExitEvent::TimeBarrierClosed)
            }
            Err(reason) => Some(ExitEvent::TimeBarrierCloseFailed { reason }),
        }
    }

    /// Monthly kill-switch: flatten, cancel everything and refuse new entries.
    ///
    /// Safe to call repeatedly: a failed attempt leaves [`HaltState::Halting`] and the next
    /// call retries.
    pub async fn halt<G: ExitGateway>(&mut self, gateway: &G) -> ExitEvent {
        if self.halt == HaltState::Halted {
            return ExitEvent::Halted;
        }
        self.halt = HaltState::Halting;
        match self.flatten(gateway).await {
            Ok(()) => {
                self.position = None;
                self.cleanup_pending = false;
                self.halt = HaltState::Halted;
                ExitEvent::Halted
            }
            Err(reason) => ExitEvent::HaltingFailed { reason },
        }
    }

    async fn try_cleanup<G: ExitGateway>(&mut self, gateway: &G) -> ExitEvent {
        let mut last_error = String::new();
        for _ in 0..MAX_ATTEMPTS {
            match gateway.cancel_orders(&self.symbol).await {
                Ok(()) => {
                    self.cleanup_pending = false;
                    return ExitEvent::CleanedUp;
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        ExitEvent::CleanupPending { reason: last_error }
    }

    /// Close, cancel, then confirm flat: a pending entry that fills in between is closed too.
    async fn flatten<G: ExitGateway>(&self, gateway: &G) -> Result<(), String> {
        let mut last_error = String::from("no attempt made");
        for _ in 0..MAX_ATTEMPTS {
            if let Err(error) = gateway.close_position(&self.symbol).await {
                last_error = error.to_string();
                continue;
            }
            if let Err(error) = gateway.cancel_orders(&self.symbol).await {
                last_error = error.to_string();
                continue;
            }
            match gateway.close_position(&self.symbol).await {
                Ok(false) => return Ok(()),
                Ok(true) => last_error = "position reappeared after cancellation".to_string(),
                Err(error) => last_error = error.to_string(),
            }
        }
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct MockGateway {
        calls: RefCell<Vec<&'static str>>,
        place: RefCell<VecDeque<Result<(), String>>>,
        cancel: RefCell<VecDeque<Result<(), String>>>,
        close: RefCell<VecDeque<Result<bool, String>>>,
    }

    impl MockGateway {
        fn calls(&self) -> Vec<&'static str> {
            self.calls.borrow().clone()
        }
    }

    impl ExitGateway for MockGateway {
        type Error = String;

        async fn place_exits(
            &self,
            _: &str,
            _: Side,
            _: Decimal,
            _: Option<Decimal>,
        ) -> Result<(), String> {
            self.calls.borrow_mut().push("place");
            self.place.borrow_mut().pop_front().unwrap_or(Ok(()))
        }

        async fn cancel_orders(&self, _: &str) -> Result<(), String> {
            self.calls.borrow_mut().push("cancel");
            self.cancel.borrow_mut().pop_front().unwrap_or(Ok(()))
        }

        async fn close_position(&self, _: &str) -> Result<bool, String> {
            self.calls.borrow_mut().push("close");
            self.close.borrow_mut().pop_front().unwrap_or(Ok(false))
        }
    }

    async fn entered(gateway: &MockGateway, max_bars: usize) -> ExitManager {
        let mut manager = ExitManager::new("BTCUSDT");
        let event = manager
            .on_entry_filled(gateway, Side::Buy, dec!(90), dec!(110), max_bars)
            .await;
        assert_eq!(event, ExitEvent::Protected);
        manager
    }

    #[tokio::test]
    async fn protection_success_blocks_new_entries_until_the_position_is_gone() {
        let gateway = MockGateway::default();
        let mut manager = entered(&gateway, 100).await;

        assert!(!manager.can_open());
        assert_eq!(gateway.calls(), vec!["place"]);

        let event = manager.on_position_closed_by_exchange(&gateway).await;

        assert_eq!(event, ExitEvent::CleanedUp);
        assert!(manager.can_open());
    }

    #[tokio::test]
    async fn failed_protection_closes_the_position_instead_of_leaving_it_naked() {
        let gateway = MockGateway::default();
        *gateway.place.borrow_mut() = VecDeque::from(vec![Err("x".to_string()); MAX_ATTEMPTS]);
        let mut manager = ExitManager::new("BTCUSDT");

        let event = manager
            .on_entry_filled(&gateway, Side::Buy, dec!(90), dec!(110), 100)
            .await;

        assert!(matches!(event, ExitEvent::UnprotectedEntryClosed { .. }));
        assert!(gateway.calls().contains(&"close"));
        assert!(manager.can_open());
    }

    #[tokio::test]
    async fn failed_protection_and_failed_close_is_reported_and_keeps_the_entry_blocked() {
        let gateway = MockGateway::default();
        *gateway.place.borrow_mut() = VecDeque::from(vec![Err("place".to_string()); MAX_ATTEMPTS]);
        *gateway.close.borrow_mut() = VecDeque::from(vec![Err("close".to_string()); MAX_ATTEMPTS]);
        let mut manager = ExitManager::new("BTCUSDT");

        let event = manager
            .on_entry_filled(&gateway, Side::Sell, dec!(110), dec!(90), 100)
            .await;

        assert!(matches!(
            event,
            ExitEvent::UnprotectedEntryCloseFailed { .. }
        ));
        assert!(!manager.can_open());
    }

    #[tokio::test]
    async fn a_naked_position_is_retried_on_every_bar_until_closed() {
        let gateway = MockGateway::default();
        *gateway.place.borrow_mut() = VecDeque::from(vec![Err("place".to_string()); MAX_ATTEMPTS]);
        *gateway.close.borrow_mut() = VecDeque::from(vec![Err("close".to_string()); MAX_ATTEMPTS]);
        let mut manager = ExitManager::new("BTCUSDT");
        manager
            .on_entry_filled(&gateway, Side::Buy, dec!(90), dec!(110), 1_000)
            .await;

        // The exchange recovers: the very next bar closes it, long before the time barrier.
        let event = manager.on_bar_closed(&gateway).await;

        assert!(matches!(
            event,
            Some(ExitEvent::UnprotectedEntryClosed { .. })
        ));
        assert!(manager.can_open());
    }

    #[tokio::test]
    async fn cleanup_failure_stays_pending_and_is_retried_on_the_next_bar() {
        let gateway = MockGateway::default();
        let mut manager = entered(&gateway, 100).await;
        *gateway.cancel.borrow_mut() = VecDeque::from(vec![Err("net".to_string()); MAX_ATTEMPTS]);

        let first = manager.on_position_closed_by_exchange(&gateway).await;
        assert!(matches!(first, ExitEvent::CleanupPending { .. }));
        assert!(!manager.can_open());

        let retry = manager.on_bar_closed(&gateway).await;
        assert_eq!(retry, Some(ExitEvent::CleanedUp));
        assert!(manager.can_open());
    }

    #[tokio::test]
    async fn time_barrier_closes_at_the_nth_bar_and_closes_before_cancelling() {
        let gateway = MockGateway::default();
        let mut manager = entered(&gateway, 2).await;

        assert_eq!(manager.on_bar_closed(&gateway).await, None);
        let event = manager.on_bar_closed(&gateway).await;

        assert_eq!(event, Some(ExitEvent::TimeBarrierClosed));
        assert_eq!(
            gateway.calls(),
            vec!["place", "close", "cancel", "close"],
            "protection must outlive the closing order"
        );
        assert!(manager.can_open());
    }

    #[tokio::test]
    async fn failed_time_barrier_close_keeps_the_position_tracked_and_retries() {
        let gateway = MockGateway::default();
        let mut manager = entered(&gateway, 1).await;
        *gateway.close.borrow_mut() = VecDeque::from(vec![Err("net".to_string()); MAX_ATTEMPTS]);

        let first = manager.on_bar_closed(&gateway).await;
        assert!(matches!(
            first,
            Some(ExitEvent::TimeBarrierCloseFailed { .. })
        ));
        assert!(!manager.can_open());

        let second = manager.on_bar_closed(&gateway).await;
        assert_eq!(second, Some(ExitEvent::TimeBarrierClosed));
    }

    #[tokio::test]
    async fn halt_flattens_confirms_flat_and_blocks_entries() {
        let gateway = MockGateway::default();
        let mut manager = entered(&gateway, 100).await;
        // First close finds a position, the confirmation read finds it flat.
        *gateway.close.borrow_mut() = VecDeque::from(vec![Ok(true), Ok(false)]);

        let event = manager.halt(&gateway).await;

        assert_eq!(event, ExitEvent::Halted);
        assert_eq!(manager.halt_state(), HaltState::Halted);
        assert!(!manager.can_open());
        assert_eq!(&gateway.calls()[1..], ["close", "cancel", "close"]);
    }

    #[tokio::test]
    async fn halt_never_claims_halted_while_the_position_may_be_open() {
        let gateway = MockGateway::default();
        let mut manager = entered(&gateway, 100).await;
        *gateway.close.borrow_mut() = VecDeque::from(vec![Err("net".to_string()); MAX_ATTEMPTS]);

        let first = manager.halt(&gateway).await;
        assert!(matches!(first, ExitEvent::HaltingFailed { .. }));
        assert_eq!(manager.halt_state(), HaltState::Halting);
        assert!(!manager.can_open());

        // The exchange recovers: the retry succeeds and only then is the session Halted.
        let second = manager.halt(&gateway).await;
        assert_eq!(second, ExitEvent::Halted);
        assert_eq!(manager.halt_state(), HaltState::Halted);
    }

    #[tokio::test]
    async fn halt_closes_an_entry_that_fills_between_cancel_and_confirmation() {
        let gateway = MockGateway::default();
        let mut manager = ExitManager::new("BTCUSDT");
        // close: nothing open, then the confirmation read finds a freshly filled entry, then flat.
        *gateway.close.borrow_mut() =
            VecDeque::from(vec![Ok(false), Ok(true), Ok(true), Ok(false)]);

        let event = manager.halt(&gateway).await;

        assert_eq!(event, ExitEvent::Halted);
    }
}
