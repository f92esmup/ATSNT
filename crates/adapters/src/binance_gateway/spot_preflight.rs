//! Spot pre-trade checks: symbol filters, free balances and the worst-case commission.

use super::*;

#[derive(Debug)]
pub(super) struct SpotSymbolInfo {
    pub(super) base_asset: String,
    pub(super) quote_asset: String,
    /// `(min_qty, step_size, min_notional)` from the exchange filters, when all are published.
    pub(super) lot_rules: Option<(Decimal, Decimal, Decimal)>,
}

#[derive(Debug)]
pub(super) struct CommissionRates {
    pub(super) maker: Decimal,
    pub(super) taker: Decimal,
    pub(super) buyer: Decimal,
    pub(super) seller: Decimal,
}

impl CommissionRates {
    pub(super) fn worst_case_for(&self, side: Side) -> Decimal {
        self.maker.max(self.taker)
            + match side {
                Side::Buy => self.buyer,
                Side::Sell => self.seller,
            }
    }
}

#[derive(Debug)]
pub(super) struct SpotCommissionInfo {
    pub(super) standard: CommissionRates,
    pub(super) special: CommissionRates,
    pub(super) tax: CommissionRates,
    pub(super) discount_enabled: bool,
    pub(super) discount_asset: String,
}

impl SpotCommissionInfo {
    pub(super) fn worst_case_components(&self, side: Side) -> (Decimal, Decimal, Decimal) {
        (
            self.standard.worst_case_for(side),
            self.special.worst_case_for(side),
            self.tax.worst_case_for(side),
        )
    }
}

impl BinanceGateway {
    pub async fn fetch_spot_balances(&self) -> Result<HashMap<String, Decimal>, GatewayError> {
        let query = self
            .auth
            .sign_query("omitZeroBalances=false", Some(self.config.recv_window_ms));
        let body = self
            .get_binance_json(&format!("/api/v3/account?{query}"))
            .await?;
        let balances = body
            .get("balances")
            .and_then(|value| value.as_array())
            .ok_or_else(|| {
                GatewayError::SpotPreflight(
                    "account response is missing the balances array".to_string(),
                )
            })?;

        let mut parsed = HashMap::with_capacity(balances.len());
        for balance in balances {
            let asset = balance
                .get("asset")
                .and_then(|value| value.as_str())
                .filter(|asset| is_valid_asset(asset))
                .ok_or_else(|| {
                    GatewayError::SpotPreflight(
                        "account response contains a missing or invalid asset".to_string(),
                    )
                })?;
            let free = parse_decimal_field(balance, "free", "account free balance")?;
            if free < Decimal::ZERO {
                return Err(GatewayError::SpotPreflight(format!(
                    "account response contains a negative free balance for {asset}"
                )));
            }
            if parsed.insert(asset.to_string(), free).is_some() {
                return Err(GatewayError::SpotPreflight(format!(
                    "account response contains duplicate balances for {asset}"
                )));
            }
        }
        Ok(parsed)
    }

    pub(super) async fn fetch_spot_symbol_info(
        &self,
        symbol: &str,
    ) -> Result<SpotSymbolInfo, GatewayError> {
        if !is_valid_asset(symbol) {
            return Err(GatewayError::SpotPreflight(
                "symbol must contain only ASCII letters and digits".to_string(),
            ));
        }
        let body = self
            .get_binance_json(&format!("/api/v3/exchangeInfo?symbol={symbol}"))
            .await?;
        let symbols = body
            .get("symbols")
            .and_then(|value| value.as_array())
            .filter(|symbols| symbols.len() == 1)
            .ok_or_else(|| {
                GatewayError::SpotPreflight(
                    "exchange information did not contain exactly one symbol".to_string(),
                )
            })?;
        let item = &symbols[0];
        if item.get("symbol").and_then(|value| value.as_str()) != Some(symbol)
            || item.get("status").and_then(|value| value.as_str()) != Some("TRADING")
            || item
                .get("isSpotTradingAllowed")
                .and_then(|value| value.as_bool())
                != Some(true)
        {
            return Err(GatewayError::SpotPreflight(format!(
                "{symbol} is not an active standard Spot symbol"
            )));
        }
        let base_asset = required_asset_field(item, "baseAsset", "exchange symbol")?;
        let quote_asset = required_asset_field(item, "quoteAsset", "exchange symbol")?;
        Ok(SpotSymbolInfo {
            base_asset,
            quote_asset,
            lot_rules: parse_spot_lot_rules(item),
        })
    }

    pub(super) async fn fetch_spot_commission_info(
        &self,
        symbol: &str,
    ) -> Result<SpotCommissionInfo, GatewayError> {
        let raw_query = format!("symbol={symbol}");
        let query = self
            .auth
            .sign_query(&raw_query, Some(self.config.recv_window_ms));
        let body = self
            .get_binance_json(&format!("/api/v3/account/commission?{query}"))
            .await?;
        if body.get("symbol").and_then(|value| value.as_str()) != Some(symbol) {
            return Err(GatewayError::SpotPreflight(
                "commission response did not match the requested symbol".to_string(),
            ));
        }
        let discount_value = body.get("discount").ok_or_else(|| {
            GatewayError::SpotPreflight(
                "commission response is missing discount settings".to_string(),
            )
        })?;
        let discount = discount_value.as_object().ok_or_else(|| {
            GatewayError::SpotPreflight(
                "commission response is missing discount settings".to_string(),
            )
        })?;
        let account_discount = discount
            .get("enabledForAccount")
            .and_then(|value| value.as_bool())
            .ok_or_else(|| {
                GatewayError::SpotPreflight(
                    "commission discount account flag is missing or malformed".to_string(),
                )
            })?;
        let symbol_discount = discount
            .get("enabledForSymbol")
            .and_then(|value| value.as_bool())
            .ok_or_else(|| {
                GatewayError::SpotPreflight(
                    "commission discount symbol flag is missing or malformed".to_string(),
                )
            })?;
        let discount_asset = required_asset_field(discount_value, "discountAsset", "commission")?;
        let discount_factor =
            parse_decimal_field(discount_value, "discount", "commission discount")?;
        if !(Decimal::ZERO..=Decimal::ONE).contains(&discount_factor) {
            return Err(GatewayError::SpotPreflight(
                "commission discount is outside the supported range".to_string(),
            ));
        }

        Ok(SpotCommissionInfo {
            standard: parse_commission_rates(&body, "standardCommission")?,
            special: parse_commission_rates(&body, "specialCommission")?,
            tax: parse_commission_rates(&body, "taxCommission")?,
            discount_enabled: account_discount && symbol_discount,
            discount_asset,
        })
    }

    pub(super) async fn fetch_spot_price(&self, symbol: &str) -> Result<Decimal, GatewayError> {
        let body = self
            .get_binance_json(&format!("/api/v3/ticker/price?symbol={symbol}"))
            .await?;
        let price = parse_decimal_field(&body, "price", "ticker price")?;
        if price <= Decimal::ZERO {
            return Err(GatewayError::SpotPreflight(
                "ticker price must be positive".to_string(),
            ));
        }
        Ok(price)
    }

    pub(super) async fn fetch_discount_asset_price_in_quote(
        &self,
        discount_asset: &str,
        symbol: &SpotSymbolInfo,
        intent: &OrderIntent,
    ) -> Result<Decimal, GatewayError> {
        if discount_asset == symbol.quote_asset {
            return Ok(Decimal::ONE);
        }
        if discount_asset == symbol.base_asset {
            return Ok(intent.price);
        }
        let direct = format!("{discount_asset}{}", symbol.quote_asset);
        match self.fetch_spot_price(&direct).await {
            Ok(price) => return Ok(price),
            Err(GatewayError::BinanceApi { code: -1121, .. }) => {}
            Err(error) => return Err(error),
        }
        let inverse = format!("{}{discount_asset}", symbol.quote_asset);
        let inverse_price = self.fetch_spot_price(&inverse).await.map_err(|_| {
            GatewayError::SpotPreflight(format!(
                "cannot value commission asset {discount_asset} in {}",
                symbol.quote_asset
            ))
        })?;
        Ok(Decimal::ONE / inverse_price)
    }

    pub(super) async fn validate_spot_preflight(
        &self,
        symbol: &str,
        intent: &OrderIntent,
        quantity: Decimal,
        symbol_info: &SpotSymbolInfo,
    ) -> Result<(), GatewayError> {
        let balances = self.fetch_spot_balances().await?;
        let notional = intent.price * quantity;
        let (required_asset, required_amount, received_amount) = match intent.side {
            Side::Buy => (&symbol_info.quote_asset, notional, quantity),
            Side::Sell => (&symbol_info.base_asset, quantity, notional),
        };
        let available = required_spot_balance(&balances, required_asset)?;
        if available < required_amount {
            return Err(GatewayError::InsufficientSpotFunds {
                asset: required_asset.clone(),
                required: required_amount,
                available,
            });
        }

        let commission = self.fetch_spot_commission_info(symbol).await?;
        let (standard_rate, special_rate, tax_rate) = commission.worst_case_components(intent.side);
        let total_rate = standard_rate + special_rate + tax_rate;
        if total_rate < Decimal::ZERO || total_rate >= Decimal::ONE {
            return Err(GatewayError::SpotPreflight(
                "commission rate cannot be safely reserved from received funds".to_string(),
            ));
        }
        let received_fee = received_amount * total_rate;

        if commission.discount_enabled && total_rate > Decimal::ZERO {
            let discount_asset_balance =
                required_spot_balance(&balances, &commission.discount_asset)?;
            if discount_asset_balance > Decimal::ZERO {
                let asset_price = self
                    .fetch_discount_asset_price_in_quote(
                        &commission.discount_asset,
                        symbol_info,
                        intent,
                    )
                    .await?;
                let fee_in_quote = received_fee
                    * if intent.side == Side::Buy {
                        intent.price
                    } else {
                        Decimal::ONE
                    };
                // Reserve against the undiscounted upper bound. Binance may instead debit
                // the received asset when the discount-asset balance cannot cover the fee.
                let required_discount_asset = fee_in_quote / asset_price;
                if discount_asset_balance >= required_discount_asset {
                    return Ok(());
                }
            }
        }

        if received_fee >= received_amount {
            return Err(GatewayError::SpotPreflight(
                "received asset cannot cover the estimated commission".to_string(),
            ));
        }
        Ok(())
    }
}

/// Reads LOT_SIZE and (MIN_)NOTIONAL from a Spot symbol; `None` unless every bound is valid.
pub(super) fn parse_spot_lot_rules(
    item: &serde_json::Value,
) -> Option<(Decimal, Decimal, Decimal)> {
    let filters = item.get("filters")?.as_array()?;
    let find = |names: &[&str]| {
        filters.iter().find(|filter| {
            filter
                .get("filterType")
                .and_then(|value| value.as_str())
                .is_some_and(|kind| names.contains(&kind))
        })
    };
    let field = |filter: &serde_json::Value, key: &str| {
        filter
            .get(key)?
            .as_str()?
            .parse::<Decimal>()
            .ok()
            .filter(|value| *value > Decimal::ZERO)
    };
    let lot = find(&["LOT_SIZE"])?;
    let notional = find(&["NOTIONAL", "MIN_NOTIONAL"])?;
    Some((
        field(lot, "minQty")?,
        field(lot, "stepSize")?,
        field(notional, "minNotional")?,
    ))
}

pub(super) fn parse_commission_rates(
    body: &serde_json::Value,
    group: &str,
) -> Result<CommissionRates, GatewayError> {
    let values = body.get(group).ok_or_else(|| {
        GatewayError::SpotPreflight(format!("commission response is missing {group}"))
    })?;
    let rates = CommissionRates {
        maker: parse_decimal_field(values, "maker", group)?,
        taker: parse_decimal_field(values, "taker", group)?,
        buyer: parse_decimal_field(values, "buyer", group)?,
        seller: parse_decimal_field(values, "seller", group)?,
    };
    if [rates.maker, rates.taker, rates.buyer, rates.seller]
        .into_iter()
        .any(|rate| rate < Decimal::ZERO)
    {
        return Err(GatewayError::SpotPreflight(format!(
            "commission response contains a negative rate in {group}"
        )));
    }
    Ok(rates)
}

pub(super) fn required_spot_balance(
    balances: &HashMap<String, Decimal>,
    asset: &str,
) -> Result<Decimal, GatewayError> {
    balances.get(asset).copied().ok_or_else(|| {
        GatewayError::SpotPreflight(format!(
            "account response is missing the required {asset} balance"
        ))
    })
}
