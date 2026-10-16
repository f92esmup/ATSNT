//! Startup (and periodic) verification of the Binance USD-M account profile the bot needs.
//!
//! The analysis is a pure function over the raw API responses, so every rule is tested
//! without a network. Leverage is deliberately never set here: it is an output of risk
//! sizing, chosen per order by the preflight.

use super::*;

/// Outcome of one profile check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingStatus {
    /// The account already matches; `detail` says what was observed.
    Ok { detail: String },
    /// The account did not match and the bot changed it (confirmed by a re-read).
    Fixed { from: String, to: String },
    /// The account does not match and the bot cannot or must not fix it.
    Blocking { reason: String, remedy: String },
}

/// Automatic correction the bot is allowed to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFix {
    /// Switch the symbol margin type to `ISOLATED`.
    SetIsolatedMargin,
}

/// One line of the account report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Short name of the checked setting.
    pub check: &'static str,
    /// Result of the check.
    pub status: FindingStatus,
    /// Correction that would resolve a blocking finding, when one is allowed.
    pub fix: Option<AccountFix>,
}

impl Finding {
    fn ok(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            status: FindingStatus::Ok {
                detail: detail.into(),
            },
            fix: None,
        }
    }

    fn blocking(check: &'static str, reason: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            check,
            status: FindingStatus::Blocking {
                reason: reason.into(),
                remedy: remedy.into(),
            },
            fix: None,
        }
    }
}

/// Everything the verification found about the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountReport {
    /// Symbol that was verified.
    pub symbol: String,
    /// One finding per checked setting, in a stable order.
    pub findings: Vec<Finding>,
}

impl AccountReport {
    /// `true` when no finding is blocking.
    pub fn is_ready(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|finding| matches!(finding.status, FindingStatus::Blocking { .. }))
    }

    /// Blocking findings only.
    pub fn blocking(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|finding| matches!(finding.status, FindingStatus::Blocking { .. }))
            .collect()
    }

    /// Human-readable table for the operator.
    pub fn render(&self) -> String {
        let mut text = format!("Account verification for {}:\n", self.symbol);
        for finding in &self.findings {
            match &finding.status {
                FindingStatus::Ok { detail } => {
                    text.push_str(&format!("  [OK]       {}: {detail}\n", finding.check));
                }
                FindingStatus::Fixed { from, to } => {
                    text.push_str(&format!("  [FIXED]    {}: {from} -> {to}\n", finding.check));
                }
                FindingStatus::Blocking { reason, remedy } => {
                    text.push_str(&format!(
                        "  [BLOCKING] {}: {reason}. To fix: {remedy}\n",
                        finding.check
                    ));
                }
            }
        }
        text
    }
}

/// Raw API responses the analysis needs.
pub(super) struct RawAccountState {
    pub(super) multi_assets: bool,
    pub(super) dual_side: bool,
    pub(super) account: serde_json::Value,
    pub(super) positions: serde_json::Value,
    pub(super) open_orders: serde_json::Value,
    pub(super) algo_orders: serde_json::Value,
    pub(super) brackets: serde_json::Value,
    pub(super) symbol_config: serde_json::Value,
}

fn array_len(value: &serde_json::Value, what: &str) -> Result<usize, GatewayError> {
    value
        .as_array()
        .map(Vec::len)
        .ok_or_else(|| GatewayError::FuturesPreflight(format!("{what} response is not an array")))
}

impl RawAccountState {
    /// `(has_position, has_orders)` for `symbol`.
    pub(super) fn exposure(&self, symbol: &str) -> Result<(bool, bool), GatewayError> {
        let rows = self.positions.as_array().ok_or_else(|| {
            GatewayError::FuturesPreflight("position response is not an array".to_string())
        })?;
        let mut has_position = false;
        for row in rows {
            if required_futures_string(row, "symbol", "position")? == symbol
                && futures_decimal_field(row, "positionAmt", "position")? != Decimal::ZERO
            {
                has_position = true;
            }
        }
        let has_orders = array_len(&self.open_orders, "open orders")? > 0
            || array_len(&self.algo_orders, "open algo orders")? > 0;
        Ok((has_position, has_orders))
    }
}

/// Pure account analysis against the profile derived from `policy`.
pub(super) fn analyze(
    state: &RawAccountState,
    symbol: &str,
    policy: &RiskPolicy,
    allow_exposure: bool,
) -> Result<Vec<Finding>, GatewayError> {
    let mut findings = Vec::new();

    findings.push(if state.dual_side {
        Finding::blocking(
            "position mode",
            "hedge mode is enabled",
            "set Position Mode to One-way in Binance Futures preferences (needs a flat account)",
        )
    } else {
        Finding::ok("position mode", "one-way")
    });

    findings.push(if state.multi_assets {
        Finding::blocking(
            "asset mode",
            "multi-assets margin mode is enabled",
            "switch to Single-Asset Mode in Binance Futures preferences (needs a flat account)",
        )
    } else {
        Finding::ok("asset mode", "single-asset (USDT)")
    });

    let assets = required_futures_array(&state.account, "assets", "futures account")?;
    let mut foreign = Vec::new();
    let mut usdt_wallet = None;
    for asset in assets {
        let name = required_futures_string(asset, "asset", "futures account asset")?;
        let wallet = futures_decimal_field(asset, "walletBalance", "futures account asset")?;
        if name == "USDT" {
            usdt_wallet = Some(wallet);
        } else if wallet != Decimal::ZERO {
            foreign.push(name.to_string());
        }
    }
    findings.push(if foreign.is_empty() {
        Finding::ok("collateral", "USDT only")
    } else {
        Finding::blocking(
            "collateral",
            format!(
                "non-USDT balances in the Futures wallet: {}",
                foreign.join(", ")
            ),
            "convert or transfer those assets out of the Futures wallet",
        )
    });

    let available = futures_decimal_field(&state.account, "availableBalance", "futures account")?;
    let wallet = usdt_wallet.unwrap_or(Decimal::ZERO);
    findings.push(if wallet > Decimal::ZERO && available > Decimal::ZERO {
        Finding::ok(
            "USDT balance",
            format!("wallet {wallet}, available {available}"),
        )
    } else {
        Finding::blocking(
            "USDT balance",
            "no positive USDT wallet or available balance",
            "transfer USDT to the Futures wallet",
        )
    });

    let (has_position, has_orders) = state.exposure(symbol)?;
    findings.push(if !has_position && !has_orders {
        Finding::ok("exposure", "flat, no open or conditional orders")
    } else if allow_exposure {
        Finding::ok(
            "exposure",
            format!(
                "open position: {has_position}, pending orders: {has_orders} (allowed while running)"
            ),
        )
    } else {
        Finding::blocking(
            "exposure",
            format!("open position: {has_position}, pending orders: {has_orders}"),
            "close the position and cancel every order for this symbol before starting",
        )
    });

    let symbol_rows = state.symbol_config.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight("symbol configuration response is not an array".to_string())
    })?;
    let config = matching_futures_symbol(symbol_rows, symbol, "symbol config")?;

    findings.push(
        if required_futures_bool_or_string(config, "isAutoAddMargin", "symbol config")? {
            Finding::blocking(
                "auto-add margin",
                "automatic margin addition is enabled",
                "turn off Auto-Add Margin for this symbol",
            )
        } else {
            Finding::ok("auto-add margin", "off")
        },
    );

    let margin_type = required_futures_string(config, "marginType", "symbol config")?;
    findings.push(if margin_type.eq_ignore_ascii_case("isolated") {
        Finding::ok("margin type", "isolated")
    } else {
        Finding {
            check: "margin type",
            status: FindingStatus::Blocking {
                reason: format!("margin type is {margin_type}"),
                remedy: "set the symbol to Isolated margin (needs no position and no orders)"
                    .to_string(),
            },
            fix: Some(AccountFix::SetIsolatedMargin),
        }
    });

    let current_leverage = required_futures_u32(config, "leverage", "symbol config")?;
    let brackets = parse_futures_leverage_brackets(&state.brackets, symbol)?;
    let expected_notional = policy.max_order_notional.min(wallet * policy.max_leverage);
    findings.push(if wallet <= Decimal::ZERO || expected_notional <= Decimal::ZERO {
        Finding::blocking(
            "leverage viability",
            "cannot estimate the largest order without a USDT balance",
            "transfer USDT to the Futures wallet",
        )
    } else {
        match bracket_for_notional(&brackets, expected_notional) {
            Err(_) => Finding::blocking(
                "leverage viability",
                format!("a {expected_notional} USDT order is outside every exchange leverage bracket"),
                "lower the configured order or position caps",
            ),
            Ok(bracket) => {
                let maintenance =
                    expected_notional * bracket.maintenance_rate - bracket.maintenance_amount;
                match minimum_sufficient_leverage(
                    expected_notional,
                    maintenance.max(Decimal::ZERO),
                    Decimal::ZERO,
                    available,
                    bracket.max_leverage,
                ) {
                    Ok(needed) => Finding::ok(
                        "leverage viability",
                        format!(
                            "the largest order ({expected_notional} USDT) needs at least {needed}x and the exchange allows {}x; leverage stays a per-order output (currently {current_leverage}x)",
                            bracket.max_leverage
                        ),
                    ),
                    Err(_) => Finding::blocking(
                        "leverage viability",
                        format!(
                            "the largest order ({expected_notional} USDT) cannot be margined even at the exchange maximum of {}x",
                            bracket.max_leverage
                        ),
                        "add USDT or lower max_order_notional / max_leverage",
                    ),
                }
            }
        }
    });

    Ok(findings)
}

impl BinanceGateway {
    async fn read_account_state(&self, symbol: &str) -> Result<RawAccountState, GatewayError> {
        let multi = self.signed_futures_path("/fapi/v1/multiAssetsMargin", "");
        let dual = self.signed_futures_path("/fapi/v1/positionSide/dual", "");
        let account = self.signed_futures_path("/fapi/v3/account", "");
        let symbol_query = format!("symbol={symbol}");
        let positions = self.signed_futures_path("/fapi/v3/positionRisk", &symbol_query);
        let orders = self.signed_futures_path("/fapi/v1/openOrders", &symbol_query);
        let algo = self.signed_futures_path("/fapi/v1/openAlgoOrders", &symbol_query);
        let brackets = self.signed_futures_path("/fapi/v1/leverageBracket", &symbol_query);
        let config = self.signed_futures_path("/fapi/v1/symbolConfig", &symbol_query);

        let (multi, dual, account, positions, open_orders, algo_orders, brackets, symbol_config) =
            tokio::try_join!(
                self.get_binance_json(&multi),
                self.get_binance_json(&dual),
                self.get_binance_json(&account),
                self.get_binance_json(&positions),
                self.get_binance_json(&orders),
                self.get_binance_json(&algo),
                self.get_binance_json(&brackets),
                self.get_binance_json(&config),
            )?;
        Ok(RawAccountState {
            multi_assets: required_futures_bool(&multi, "multiAssetsMargin", "account mode")?,
            dual_side: required_futures_bool(&dual, "dualSidePosition", "position mode")?,
            account,
            positions,
            open_orders,
            algo_orders,
            brackets,
            symbol_config,
        })
    }

    /// Verifies the account against the profile the strategy needs and reports every finding.
    ///
    /// `allow_exposure` is for periodic checks while a position or order may legitimately
    /// exist. Nothing is ever changed in that mode.
    pub async fn verify_account(
        &self,
        symbol: &str,
        _auto_fix: bool,
        allow_exposure: bool,
    ) -> Result<AccountReport, GatewayError> {
        if self.config.spot {
            return Err(GatewayError::Config(
                "account verification is only supported for USD-M futures".to_string(),
            ));
        }
        if !is_valid_asset(symbol) {
            return Err(GatewayError::Config(
                "symbol must contain only ASCII letters and digits".to_string(),
            ));
        }
        let state = self.read_account_state(symbol).await?;
        let findings = analyze(&state, symbol, &self.config.risk_policy, allow_exposure)?;
        Ok(AccountReport {
            symbol: symbol.to_string(),
            findings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good_state() -> RawAccountState {
        RawAccountState {
            multi_assets: false,
            dual_side: false,
            account: json!({
                "availableBalance": "5000",
                "assets": [
                    {"asset": "USDT", "walletBalance": "5000", "availableBalance": "5000"},
                    {"asset": "BNB", "walletBalance": "0", "availableBalance": "0"}
                ]
            }),
            positions: json!([{"symbol": "BTCUSDT", "positionSide": "BOTH", "positionAmt": "0"}]),
            open_orders: json!([]),
            algo_orders: json!([]),
            brackets: json!([{
                "symbol": "BTCUSDT",
                "brackets": [{
                    "bracket": 1, "initialLeverage": 125, "notionalCap": 50000,
                    "notionalFloor": 0, "maintMarginRatio": 0.004, "cum": 0
                }]
            }]),
            symbol_config: json!([{
                "symbol": "BTCUSDT", "marginType": "ISOLATED", "isAutoAddMargin": "false",
                "leverage": 20, "maxNotionalValue": "1000000"
            }]),
        }
    }

    fn run(state: &RawAccountState, allow_exposure: bool) -> Vec<Finding> {
        analyze(state, "BTCUSDT", &RiskPolicy::default(), allow_exposure).unwrap()
    }

    fn blocking_checks(findings: &[Finding]) -> Vec<&'static str> {
        findings
            .iter()
            .filter(|f| matches!(f.status, FindingStatus::Blocking { .. }))
            .map(|f| f.check)
            .collect()
    }

    #[test]
    fn a_matching_account_has_no_blocking_findings() {
        let findings = run(&good_state(), false);

        assert!(blocking_checks(&findings).is_empty(), "{findings:?}");
        assert_eq!(findings.len(), 8);
    }

    #[test]
    fn hedge_and_multi_asset_modes_are_blocking_and_not_fixable() {
        let mut state = good_state();
        state.dual_side = true;
        state.multi_assets = true;

        let findings = run(&state, false);

        assert_eq!(
            blocking_checks(&findings),
            vec!["position mode", "asset mode"]
        );
        assert!(findings.iter().all(|f| f.fix.is_none()));
    }

    #[test]
    fn foreign_collateral_is_blocking_and_names_the_assets() {
        let mut state = good_state();
        state.account["assets"] = json!([
            {"asset": "USDT", "walletBalance": "5000", "availableBalance": "5000"},
            {"asset": "BTC", "walletBalance": "1.0", "availableBalance": "1.0"},
            {"asset": "ETH", "walletBalance": "2.0", "availableBalance": "2.0"}
        ]);

        let findings = run(&state, false);

        let collateral = findings.iter().find(|f| f.check == "collateral").unwrap();
        match &collateral.status {
            FindingStatus::Blocking { reason, .. } => {
                assert!(reason.contains("BTC") && reason.contains("ETH"), "{reason}");
            }
            other => panic!("expected blocking, got {other:?}"),
        }
    }

    #[test]
    fn cross_margin_is_blocking_but_marked_fixable() {
        let mut state = good_state();
        state.symbol_config[0]["marginType"] = json!("CROSSED");

        let findings = run(&state, false);

        let margin = findings.iter().find(|f| f.check == "margin type").unwrap();
        assert!(matches!(margin.status, FindingStatus::Blocking { .. }));
        assert_eq!(margin.fix, Some(AccountFix::SetIsolatedMargin));
    }

    #[test]
    fn existing_position_or_orders_block_startup_but_not_periodic_checks() {
        let mut state = good_state();
        state.positions =
            json!([{"symbol": "BTCUSDT", "positionSide": "BOTH", "positionAmt": "0.01"}]);
        state.algo_orders = json!([{"algoId": 1}]);

        assert_eq!(blocking_checks(&run(&state, false)), vec!["exposure"]);
        assert!(blocking_checks(&run(&state, true)).is_empty());
    }

    #[test]
    fn auto_add_margin_is_blocking() {
        let mut state = good_state();
        state.symbol_config[0]["isAutoAddMargin"] = json!("true");

        assert_eq!(
            blocking_checks(&run(&state, false)),
            vec!["auto-add margin"]
        );
    }

    #[test]
    fn leverage_is_checked_for_viability_and_never_set() {
        let findings = run(&good_state(), false);

        let leverage = findings
            .iter()
            .find(|f| f.check == "leverage viability")
            .unwrap();
        match &leverage.status {
            FindingStatus::Ok { detail } => {
                assert!(detail.contains("per-order output"), "{detail}");
                assert!(detail.contains("125x"), "{detail}");
            }
            other => panic!("expected ok, got {other:?}"),
        }
        assert!(leverage.fix.is_none());
    }

    #[test]
    fn a_tiny_wallet_with_a_low_exchange_maximum_is_not_viable() {
        let mut state = good_state();
        state.account = json!({
            "availableBalance": "1",
            "assets": [{"asset": "USDT", "walletBalance": "1", "availableBalance": "1"}]
        });
        state.brackets[0]["brackets"][0]["initialLeverage"] = json!(1);

        let findings = run(&state, false);

        assert_eq!(blocking_checks(&findings), vec!["leverage viability"]);
    }

    #[test]
    fn the_report_renders_every_state_and_is_ready_only_without_blockers() {
        let ready = AccountReport {
            symbol: "BTCUSDT".to_string(),
            findings: run(&good_state(), false),
        };
        assert!(ready.is_ready());
        assert!(ready.render().contains("[OK]"));

        let mut state = good_state();
        state.dual_side = true;
        let blocked = AccountReport {
            symbol: "BTCUSDT".to_string(),
            findings: run(&state, false),
        };
        assert!(!blocked.is_ready());
        assert_eq!(blocked.blocking().len(), 1);
        assert!(blocked.render().contains("[BLOCKING] position mode"));
        assert!(blocked.render().contains("To fix:"));
    }
}
