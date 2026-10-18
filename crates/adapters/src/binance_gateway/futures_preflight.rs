//! USD-M pre-trade checks: the account snapshot, contract filters, leverage brackets and the
//! freshness guards applied before an order may be dispatched.

use super::*;

pub(super) const FUTURES_MAX_DATA_AGE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug)]
pub(super) struct FuturesSymbolRules {
    pub(super) min_price: Decimal,
    pub(super) max_price: Decimal,
    pub(super) tick_size: Decimal,
    pub(super) min_qty: Decimal,
    pub(super) max_qty: Decimal,
    pub(super) step_size: Decimal,
    pub(super) min_notional: Decimal,
}

#[derive(Debug)]
pub(super) struct FuturesLeverageBracket {
    pub(super) notional_floor: Decimal,
    pub(super) notional_cap: Decimal,
    pub(super) maintenance_rate: Decimal,
    pub(super) maintenance_amount: Decimal,
    pub(super) max_leverage: u32,
}

#[derive(Debug)]
pub(super) struct FuturesPreflightSnapshot {
    pub(super) observed_at: [std::time::Instant; 5],
    pub(super) available_balance: Decimal,
    pub(super) position_notional: Decimal,
    pub(super) isolated_wallet: Decimal,
    pub(super) current_leverage: u32,
    pub(super) maximum_notional: Decimal,
    pub(super) margin_type: String,
    pub(super) open_order_notional: Decimal,
    pub(super) has_position: bool,
    pub(super) has_open_orders: bool,
    pub(super) mark_time_ms: u64,
    /// Local wall-clock time at which the mark price response was received. The mark's age is
    /// judged then; the time since is bounded by the snapshot age guard.
    pub(super) mark_received_ms: u64,
    pub(super) rules: FuturesSymbolRules,
    pub(super) leverage_brackets: Vec<FuturesLeverageBracket>,
    pub(super) taker_commission_rate: Decimal,
}

impl BinanceGateway {
    pub(super) async fn fetch_futures_preflight_snapshot(
        &self,
        symbol: &str,
    ) -> Result<FuturesPreflightSnapshot, GatewayError> {
        if self.config.spot {
            return Err(GatewayError::FuturesPreflight(
                "USD-M preflight is unavailable in Spot mode".to_string(),
            ));
        }
        if !is_valid_asset(symbol) {
            return Err(GatewayError::FuturesPreflight(
                "symbol must contain only ASCII letters and digits".to_string(),
            ));
        }

        let multi_assets_path = self.signed_futures_path("/fapi/v1/multiAssetsMargin", "");
        let position_mode_path = self.signed_futures_path("/fapi/v1/positionSide/dual", "");
        let account_path = self.signed_futures_path("/fapi/v3/account", "");
        let positions_path =
            self.signed_futures_path("/fapi/v3/positionRisk", &format!("symbol={symbol}"));
        let orders_path =
            self.signed_futures_path("/fapi/v1/openOrders", &format!("symbol={symbol}"));
        let algo_orders_path =
            self.signed_futures_path("/fapi/v1/openAlgoOrders", &format!("symbol={symbol}"));
        let bracket_path =
            self.signed_futures_path("/fapi/v1/leverageBracket", &format!("symbol={symbol}"));
        let commission_path =
            self.signed_futures_path("/fapi/v1/commissionRate", &format!("symbol={symbol}"));
        let symbol_config_path =
            self.signed_futures_path("/fapi/v1/symbolConfig", &format!("symbol={symbol}"));
        let exchange_info_path = "/fapi/v1/exchangeInfo".to_string();
        let mark_price_path = format!("/fapi/v1/premiumIndex?symbol={symbol}");

        let (
            multi_assets,
            position_mode,
            account,
            positions,
            open_orders,
            algo_orders,
            exchange_info,
            mark_price,
            leverage_brackets,
            commission,
            symbol_config,
        ) = tokio::try_join!(
            self.get_binance_json(&multi_assets_path),
            self.get_binance_json(&position_mode_path),
            self.get_observed_binance_json(&account_path),
            self.get_observed_binance_json(&positions_path),
            self.get_observed_binance_json(&orders_path),
            self.get_observed_binance_json(&algo_orders_path),
            self.get_binance_json(&exchange_info_path),
            async {
                let (body, observed_at) = self.get_observed_binance_json(&mark_price_path).await?;
                Ok::<_, GatewayError>((body, observed_at, current_unix_time_ms()?))
            },
            self.get_binance_json(&bracket_path),
            self.get_binance_json(&commission_path),
            self.get_binance_json(&symbol_config_path),
        )?;

        if required_futures_bool(&multi_assets, "multiAssetsMargin", "account mode")? {
            return Err(GatewayError::FuturesPreflight(
                "multi-asset margin mode is unsupported; use single-asset USDT mode".to_string(),
            ));
        }
        if required_futures_bool(&position_mode, "dualSidePosition", "position mode")? {
            return Err(GatewayError::FuturesPreflight(
                "hedge mode is unsupported; use one-way position mode".to_string(),
            ));
        }

        let observed_at = [
            account.1,
            positions.1,
            open_orders.1,
            algo_orders.1,
            mark_price.1,
        ];
        parse_futures_preflight_snapshot(FuturesPreflightInput {
            symbol,
            observed_at,
            account: &account.0,
            positions: &positions.0,
            open_orders: &open_orders.0,
            algo_orders: &algo_orders.0,
            exchange_info: &exchange_info,
            mark: &mark_price.0,
            leverage_brackets: &leverage_brackets,
            commission: &commission,
            symbol_config: &symbol_config,
            mark_received_ms: mark_price.2,
            allow_foreign_collateral: self.config.allow_foreign_collateral,
        })
    }

    pub(super) async fn validate_futures_preflight(
        &self,
        symbol: &str,
        intent: &OrderIntent,
        quantity: Decimal,
        snapshot: &FuturesPreflightSnapshot,
    ) -> Result<u32, GatewayError> {
        validate_futures_snapshot_age(&snapshot.observed_at, std::time::Instant::now())?;
        validate_futures_mark_timestamp(snapshot.mark_time_ms, current_unix_time_ms()?)?;
        validate_futures_order_rules(&snapshot.rules, intent.price, quantity)?;

        let order_notional = intent.price * quantity;
        let projected_notional =
            snapshot.position_notional + snapshot.open_order_notional + order_notional;
        if order_notional > self.config.risk_policy.max_order_notional {
            return Err(GatewayError::FuturesPreflight(format!(
                "order notional {order_notional} exceeds the configured cap {}",
                self.config.risk_policy.max_order_notional
            )));
        }
        if projected_notional > self.config.risk_policy.max_position_notional {
            return Err(GatewayError::FuturesPreflight(format!(
                "projected symbol exposure {projected_notional} exceeds the configured cap {}",
                self.config.risk_policy.max_position_notional
            )));
        }

        let bracket = bracket_for_notional(&snapshot.leverage_brackets, projected_notional)?;
        let maintenance_margin =
            projected_notional * bracket.maintenance_rate - bracket.maintenance_amount;
        if maintenance_margin < Decimal::ZERO {
            return Err(GatewayError::FuturesPreflight(
                "leverage bracket produced a negative maintenance margin".to_string(),
            ));
        }
        let commission = order_notional * snapshot.taker_commission_rate;
        let has_existing_exposure = snapshot.has_position || snapshot.has_open_orders;

        let leverage = if has_existing_exposure {
            snapshot.current_leverage
        } else {
            minimum_sufficient_leverage(
                projected_notional,
                maintenance_margin,
                commission,
                snapshot.available_balance,
                bracket.max_leverage,
            )?
        };
        if leverage == 0 || leverage > bracket.max_leverage {
            return Err(GatewayError::FuturesPreflight(
                "current leverage exceeds the maximum allowed by the active notional bracket"
                    .to_string(),
            ));
        }
        if has_existing_exposure && projected_notional > snapshot.maximum_notional {
            return Err(GatewayError::FuturesPreflight(format!(
                "projected symbol exposure {projected_notional} exceeds the configured leverage notional limit {}",
                snapshot.maximum_notional
            )));
        }

        let additional_initial_margin = order_notional / Decimal::from(leverage);
        if snapshot.available_balance < additional_initial_margin + commission {
            return Err(GatewayError::FuturesPreflight(format!(
                "available USDT balance {} cannot cover initial margin and fees {}",
                snapshot.available_balance,
                additional_initial_margin + commission
            )));
        }
        if snapshot.available_balance + snapshot.isolated_wallet < maintenance_margin + commission {
            return Err(GatewayError::FuturesPreflight(
                "available and isolated USDT margin cannot cover projected maintenance margin and fees"
                    .to_string(),
            ));
        }

        if !snapshot.margin_type.eq_ignore_ascii_case("isolated") {
            return Err(GatewayError::FuturesPreflight(
                "cross margin is unsupported; configure this symbol as isolated before trading"
                    .to_string(),
            ));
        }

        if !has_existing_exposure && leverage != snapshot.current_leverage {
            let response = self
                .post_signed_futures_json(
                    "/fapi/v1/leverage",
                    &format!("symbol={symbol}&leverage={leverage}"),
                )
                .await?;
            let applied_leverage =
                required_futures_u32(&response, "leverage", "leverage update response")?;
            let applied_symbol =
                required_futures_string(&response, "symbol", "leverage update response")?;
            let applied_maximum_notional =
                futures_decimal_field(&response, "maxNotionalValue", "leverage update response")?;
            if applied_leverage != leverage
                || applied_symbol != symbol
                || projected_notional > applied_maximum_notional
            {
                return Err(GatewayError::FuturesPreflight(format!(
                    "exchange leverage update did not support the requested {leverage}x exposure"
                )));
            }
        } else if projected_notional > snapshot.maximum_notional {
            return Err(GatewayError::FuturesPreflight(format!(
                "projected symbol exposure {projected_notional} exceeds the configured leverage notional limit {}",
                snapshot.maximum_notional
            )));
        }

        validate_futures_snapshot_age(&snapshot.observed_at, std::time::Instant::now())?;
        Ok(leverage)
    }
}

pub(super) fn parse_futures_symbol_rules(
    body: &serde_json::Value,
    symbol: &str,
) -> Result<FuturesSymbolRules, GatewayError> {
    let symbols = required_futures_array(body, "symbols", "exchange information")?;
    let item = matching_futures_symbol(symbols, symbol, "exchange information")?;
    if required_futures_string(item, "status", "exchange symbol")? != "TRADING"
        || required_futures_string(item, "contractType", "exchange symbol")? != "PERPETUAL"
        || required_futures_string(item, "quoteAsset", "exchange symbol")? != "USDT"
        || required_futures_string(item, "marginAsset", "exchange symbol")? != "USDT"
    {
        return Err(GatewayError::FuturesPreflight(format!(
            "{symbol} is not an active USDT-margined perpetual contract"
        )));
    }
    let filters = required_futures_array(item, "filters", "exchange symbol")?;
    let filter = |name: &str| -> Result<&serde_json::Value, GatewayError> {
        let mut matches = filters
            .iter()
            .filter(|entry| entry.get("filterType").and_then(|value| value.as_str()) == Some(name));
        let found = matches.next().ok_or_else(|| {
            GatewayError::FuturesPreflight(format!("exchange symbol is missing the {name} filter"))
        })?;
        if matches.next().is_some() {
            return Err(GatewayError::FuturesPreflight(format!(
                "exchange symbol contains duplicate {name} filters"
            )));
        }
        Ok(found)
    };

    let price_filter = filter("PRICE_FILTER")?;
    let quantity_filter = filter("LOT_SIZE")?;
    let notional_filter = filter("MIN_NOTIONAL")?;
    let rules = FuturesSymbolRules {
        min_price: futures_decimal_field(price_filter, "minPrice", "price filter")?,
        max_price: futures_decimal_field(price_filter, "maxPrice", "price filter")?,
        tick_size: futures_decimal_field(price_filter, "tickSize", "price filter")?,
        min_qty: futures_decimal_field(quantity_filter, "minQty", "quantity filter")?,
        max_qty: futures_decimal_field(quantity_filter, "maxQty", "quantity filter")?,
        step_size: futures_decimal_field(quantity_filter, "stepSize", "quantity filter")?,
        min_notional: futures_decimal_field(notional_filter, "notional", "notional filter")?,
    };
    if rules.min_price <= Decimal::ZERO
        || rules.max_price < rules.min_price
        || rules.tick_size <= Decimal::ZERO
        || rules.min_qty <= Decimal::ZERO
        || rules.max_qty < rules.min_qty
        || rules.step_size <= Decimal::ZERO
        || rules.min_notional <= Decimal::ZERO
    {
        return Err(GatewayError::FuturesPreflight(
            "exchange symbol filters contain invalid bounds".to_string(),
        ));
    }
    Ok(rules)
}

pub(super) fn parse_futures_leverage_brackets(
    body: &serde_json::Value,
    symbol: &str,
) -> Result<Vec<FuturesLeverageBracket>, GatewayError> {
    let items = body.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "leverage bracket response is missing the result array".to_string(),
        )
    })?;
    let symbol_entry = matching_futures_symbol(items, symbol, "leverage bracket")?;
    let rows = required_futures_array(symbol_entry, "brackets", "leverage bracket")?;
    if rows.is_empty() {
        return Err(GatewayError::FuturesPreflight(
            "leverage bracket response contains no brackets".to_string(),
        ));
    }
    let mut brackets = Vec::with_capacity(rows.len());
    for row in rows {
        let floor = futures_decimal_field(row, "notionalFloor", "leverage bracket")?;
        let cap = futures_decimal_field(row, "notionalCap", "leverage bracket")?;
        let rate = futures_decimal_field(row, "maintMarginRatio", "leverage bracket")?;
        let amount = futures_decimal_field(row, "cum", "leverage bracket")?;
        let max_leverage = required_futures_u32(row, "initialLeverage", "leverage bracket")?;
        if floor < Decimal::ZERO
            || cap <= floor
            || !(Decimal::ZERO..Decimal::ONE).contains(&rate)
            || amount < Decimal::ZERO
            || max_leverage == 0
        {
            return Err(GatewayError::FuturesPreflight(
                "leverage bracket response contains invalid bounds".to_string(),
            ));
        }
        brackets.push(FuturesLeverageBracket {
            notional_floor: floor,
            notional_cap: cap,
            maintenance_rate: rate,
            maintenance_amount: amount,
            max_leverage,
        });
    }
    brackets.sort_by_key(|bracket| bracket.notional_floor);
    if brackets
        .windows(2)
        .any(|pair| pair[0].notional_floor == pair[1].notional_floor)
    {
        return Err(GatewayError::FuturesPreflight(
            "leverage bracket response contains duplicate notional tiers".to_string(),
        ));
    }
    Ok(brackets)
}

pub(super) struct FuturesPreflightInput<'a> {
    pub(super) symbol: &'a str,
    pub(super) observed_at: [std::time::Instant; 5],
    pub(super) account: &'a serde_json::Value,
    pub(super) positions: &'a serde_json::Value,
    pub(super) open_orders: &'a serde_json::Value,
    pub(super) algo_orders: &'a serde_json::Value,
    pub(super) exchange_info: &'a serde_json::Value,
    pub(super) mark: &'a serde_json::Value,
    pub(super) leverage_brackets: &'a serde_json::Value,
    pub(super) commission: &'a serde_json::Value,
    pub(super) symbol_config: &'a serde_json::Value,
    pub(super) mark_received_ms: u64,
    pub(super) allow_foreign_collateral: bool,
}

pub(super) fn parse_futures_preflight_snapshot(
    input: FuturesPreflightInput<'_>,
) -> Result<FuturesPreflightSnapshot, GatewayError> {
    let FuturesPreflightInput {
        symbol,
        observed_at,
        account,
        positions,
        open_orders,
        algo_orders,
        exchange_info,
        mark,
        leverage_brackets,
        commission,
        symbol_config,
        mark_received_ms,
        allow_foreign_collateral,
    } = input;

    // `/fapi/v3/account` does not return `canTrade`: reject only when it is present and false.
    if account.get("canTrade").is_some()
        && !required_futures_bool(account, "canTrade", "futures account")?
    {
        return Err(GatewayError::FuturesPreflight(
            "futures account is not enabled for trading".to_string(),
        ));
    }
    let assets = required_futures_array(account, "assets", "futures account")?;
    let mut usdt_assets = assets
        .iter()
        .filter(|asset| asset.get("asset").and_then(|value| value.as_str()) == Some("USDT"));
    let usdt_asset = usdt_assets.next().ok_or_else(|| {
        GatewayError::FuturesPreflight("futures account is missing USDT collateral".to_string())
    })?;
    if usdt_assets.next().is_some() {
        return Err(GatewayError::FuturesPreflight(
            "futures account contains duplicate USDT collateral entries".to_string(),
        ));
    }
    let available_balance =
        futures_decimal_field(usdt_asset, "availableBalance", "USDT account balance")?;
    let account_available = futures_decimal_field(account, "availableBalance", "futures account")?;
    let usdt_wallet = futures_decimal_field(usdt_asset, "walletBalance", "USDT account balance")?;
    if available_balance != account_available
        || available_balance < Decimal::ZERO
        || usdt_wallet < Decimal::ZERO
    {
        return Err(GatewayError::FuturesPreflight(
            "account-level and USDT available balances are inconsistent".to_string(),
        ));
    }
    for asset in assets {
        if !allow_foreign_collateral
            && asset.get("asset").and_then(|value| value.as_str()) != Some("USDT")
            && futures_decimal_field(asset, "walletBalance", "futures account asset")?
                != Decimal::ZERO
        {
            return Err(GatewayError::FuturesPreflight(
                "non-USDT futures collateral is unsupported in the selected account profile"
                    .to_string(),
            ));
        }
    }

    let position_rows = positions.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight("position response is missing the result array".to_string())
    })?;
    let (position_amount, isolated_wallet, exchange_position_notional) = if position_rows.is_empty()
    {
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
    } else {
        let position = matching_futures_symbol(position_rows, symbol, "position")?;
        if required_futures_string(position, "positionSide", "position")? != "BOTH" {
            return Err(GatewayError::FuturesPreflight(
                "position response is not in one-way mode".to_string(),
            ));
        }
        let position_amount = futures_decimal_field(position, "positionAmt", "position")?;
        let isolated_wallet = futures_decimal_field(position, "isolatedWallet", "position")?;
        let exchange_notional = futures_decimal_field(position, "notional", "position")?.abs();
        if required_futures_string(position, "marginAsset", "position")? != "USDT"
            || isolated_wallet < Decimal::ZERO
        {
            return Err(GatewayError::FuturesPreflight(
                "position response contains unsupported margin asset or invalid isolated margin"
                    .to_string(),
            ));
        }
        (position_amount, isolated_wallet, exchange_notional)
    };
    let position_notional = (position_amount.abs()
        * futures_decimal_field(mark, "markPrice", "mark price")?)
    .max(exchange_position_notional);
    if position_amount == Decimal::ZERO && exchange_position_notional != Decimal::ZERO {
        return Err(GatewayError::FuturesPreflight(
            "position amount and reported notional are inconsistent".to_string(),
        ));
    }
    if position_amount == Decimal::ZERO && exchange_position_notional != Decimal::ZERO {
        return Err(GatewayError::FuturesPreflight(
            "position amount and reported notional are inconsistent".to_string(),
        ));
    }
    let has_position = position_amount != Decimal::ZERO;

    let algo_orders = algo_orders.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "algorithmic open-order response is missing the result array".to_string(),
        )
    })?;
    if !algo_orders.is_empty() {
        return Err(GatewayError::FuturesPreflight(
            "open conditional orders are unsupported in the selected Futures profile".to_string(),
        ));
    }

    let orders = open_orders.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "open-order response is missing the result array".to_string(),
        )
    })?;
    let mut open_order_notional = Decimal::ZERO;
    for order in orders {
        if required_futures_string(order, "symbol", "open order")? != symbol
            || required_futures_string(order, "positionSide", "open order")? != "BOTH"
            || required_futures_string(order, "type", "open order")? != "LIMIT"
            || !matches!(
                required_futures_string(order, "side", "open order")?,
                "BUY" | "SELL"
            )
        {
            return Err(GatewayError::FuturesPreflight(
                "open orders contain an unsupported symbol, position side, or order type"
                    .to_string(),
            ));
        }
        let price = futures_decimal_field(order, "price", "open order")?;
        let original = futures_decimal_field(order, "origQty", "open order")?;
        let executed = futures_decimal_field(order, "executedQty", "open order")?;
        let remaining = original - executed;
        if price <= Decimal::ZERO
            || original <= Decimal::ZERO
            || executed < Decimal::ZERO
            || remaining <= Decimal::ZERO
        {
            return Err(GatewayError::FuturesPreflight(
                "open order contains invalid price or remaining quantity".to_string(),
            ));
        }
        open_order_notional += price * remaining;
    }

    let mark_price = futures_decimal_field(mark, "markPrice", "mark price")?;
    let mark_time_ms = required_futures_timestamp(mark, "time", "mark price")?;
    if mark_price <= Decimal::ZERO {
        return Err(GatewayError::FuturesPreflight(
            "mark price must be positive".to_string(),
        ));
    }
    validate_futures_mark_timestamp(mark_time_ms, mark_received_ms)?;

    let brackets = parse_futures_leverage_brackets(leverage_brackets, symbol)?;
    let commission_symbol = required_futures_string(commission, "symbol", "commission")?;
    if commission_symbol != symbol {
        return Err(GatewayError::FuturesPreflight(
            "commission response symbol does not match the order symbol".to_string(),
        ));
    }
    let maker_rate = futures_decimal_field(commission, "makerCommissionRate", "commission")?;
    let taker_rate = futures_decimal_field(commission, "takerCommissionRate", "commission")?;
    if maker_rate < Decimal::ZERO
        || taker_rate < Decimal::ZERO
        || maker_rate >= Decimal::ONE
        || taker_rate >= Decimal::ONE
    {
        return Err(GatewayError::FuturesPreflight(
            "commission rates are outside the safe supported range".to_string(),
        ));
    }

    let symbol_config_rows = symbol_config.as_array().ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "symbol configuration response is missing the result array".to_string(),
        )
    })?;
    let symbol_config = matching_futures_symbol(symbol_config_rows, symbol, "symbol config")?;
    let margin_type = required_futures_string(symbol_config, "marginType", "symbol config")?;
    let current_leverage = required_futures_u32(symbol_config, "leverage", "symbol config")?;
    let maximum_notional =
        futures_decimal_field(symbol_config, "maxNotionalValue", "symbol config")?;
    // Leverage 0 means "never set on this account" (testnet reports it for untouched symbols).
    // While flat the gateway chooses and sets the leverage of the order itself, so it is only
    // unusable when a position or orders already run on the exchange's current leverage.
    let leverage_unusable = current_leverage == 0 && (has_position || !orders.is_empty());
    if leverage_unusable || maximum_notional <= Decimal::ZERO {
        tracing::warn!(
            current_leverage,
            maximum_notional = %maximum_notional,
            has_position,
            has_open_orders = !orders.is_empty(),
            "symbol configuration refused: invalid leverage or notional bounds"
        );
        return Err(GatewayError::FuturesPreflight(
            "symbol configuration contains invalid leverage or notional bounds".to_string(),
        ));
    }
    if required_futures_bool_or_string(symbol_config, "isAutoAddMargin", "symbol config")? {
        return Err(GatewayError::FuturesPreflight(
            "automatic margin addition is unsupported in the selected isolated profile".to_string(),
        ));
    }

    Ok(FuturesPreflightSnapshot {
        observed_at,
        available_balance,
        position_notional,
        isolated_wallet,
        current_leverage,
        maximum_notional,
        margin_type: margin_type.to_string(),
        open_order_notional,
        has_position,
        has_open_orders: !orders.is_empty(),
        mark_time_ms,
        mark_received_ms,
        rules: parse_futures_symbol_rules(exchange_info, symbol)?,
        leverage_brackets: brackets,
        taker_commission_rate: maker_rate.max(taker_rate),
    })
}

pub(super) fn validate_futures_order_rules(
    rules: &FuturesSymbolRules,
    price: Decimal,
    quantity: Decimal,
) -> Result<(), GatewayError> {
    let notional = price * quantity;
    // `price` is the reference of a market order, never sent to the exchange, so it only has to
    // lie inside the contract bounds; tick alignment applies to limit prices only.
    if price < rules.min_price || price > rules.max_price {
        return Err(GatewayError::FuturesPreflight(
            "order price violates the contract price filter".to_string(),
        ));
    }
    if quantity < rules.min_qty
        || quantity > rules.max_qty
        || quantity % rules.step_size != Decimal::ZERO
    {
        return Err(GatewayError::FuturesPreflight(
            "order quantity violates the contract lot-size filter".to_string(),
        ));
    }
    if notional < rules.min_notional {
        return Err(GatewayError::FuturesPreflight(
            "order notional is below the contract minimum".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_futures_snapshot_age(
    observations: &[std::time::Instant],
    dispatch_at: std::time::Instant,
) -> Result<(), GatewayError> {
    if observations.iter().any(|observed_at| {
        dispatch_at < *observed_at
            || dispatch_at.duration_since(*observed_at) > FUTURES_MAX_DATA_AGE
    }) {
        return Err(GatewayError::FuturesPreflight(
            "account, position, open-order, or mark-price data is older than two seconds"
                .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_futures_mark_timestamp(
    mark_time_ms: u64,
    now_ms: u64,
) -> Result<(), GatewayError> {
    if mark_time_ms > now_ms || now_ms - mark_time_ms > 2_000 {
        // Positive: how old the mark price is; negative: how far ahead of the local clock. The
        // refusal text stays as it always was; the numbers go to the log for diagnosis.
        let age_ms = i128::from(now_ms) - i128::from(mark_time_ms);
        tracing::warn!(
            mark_time_ms,
            received_ms = now_ms,
            age_ms = %age_ms,
            "mark price refused: stale or in the future (limit 2000 ms when received)"
        );
        return Err(GatewayError::FuturesPreflight(
            "mark-price exchange timestamp is stale or in the future".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn current_unix_time_ms() -> Result<u64, GatewayError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|_| {
            GatewayError::FuturesPreflight("system clock is before the Unix epoch".to_string())
        })
}

pub(super) fn bracket_for_notional(
    brackets: &[FuturesLeverageBracket],
    notional: Decimal,
) -> Result<&FuturesLeverageBracket, GatewayError> {
    brackets
        .iter()
        .filter(|bracket| notional >= bracket.notional_floor && notional <= bracket.notional_cap)
        .max_by(|left, right| left.notional_floor.cmp(&right.notional_floor))
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(
                "projected symbol exposure is outside all exchange leverage brackets".to_string(),
            )
        })
}

pub(super) fn minimum_sufficient_leverage(
    notional: Decimal,
    maintenance_margin: Decimal,
    commission: Decimal,
    available_balance: Decimal,
    maximum_leverage: u32,
) -> Result<u32, GatewayError> {
    if available_balance <= Decimal::ZERO || maximum_leverage == 0 {
        return Err(GatewayError::FuturesPreflight(
            "no positive USDT balance or supported leverage is available".to_string(),
        ));
    }
    domain::futures::minimum_leverage(
        notional,
        maintenance_margin,
        commission,
        available_balance,
        maximum_leverage,
    )
    .ok_or_else(|| {
        GatewayError::FuturesPreflight(
            "available USDT balance cannot cover exposure at any supported leverage".to_string(),
        )
    })
}
