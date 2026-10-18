//! Strict readers for the fields of Binance JSON responses.
//!
//! Each reader names the response and the field in its error, so a malformed exchange answer
//! is refused with a precise reason instead of a default value.

use super::*;

pub(super) fn is_valid_asset(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

pub(super) fn required_asset_field(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<String, GatewayError> {
    let asset = body
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| is_valid_asset(value))
        .ok_or_else(|| {
            GatewayError::SpotPreflight(format!(
                "{context} response is missing or has an invalid {key}"
            ))
        })?;
    Ok(asset.to_string())
}

pub(super) fn parse_decimal_field(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<Decimal, GatewayError> {
    let value = body.get(key).ok_or_else(|| {
        GatewayError::SpotPreflight(format!("{context} response is missing {key}"))
    })?;
    let raw = value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_number().map(ToString::to_string))
        .ok_or_else(|| {
            GatewayError::SpotPreflight(format!("{context} response has malformed {key}"))
        })?;
    Decimal::from_str(&raw)
        .or_else(|_| Decimal::from_scientific(&raw))
        .map_err(GatewayError::Decimal)
}

pub(super) fn futures_decimal_field(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<Decimal, GatewayError> {
    let value = body.get(key).ok_or_else(|| {
        GatewayError::FuturesPreflight(format!("{context} response is missing {key}"))
    })?;
    let raw = value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_number().map(ToString::to_string))
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!("{context} response has malformed {key}"))
        })?;
    Decimal::from_str(&raw)
        .or_else(|_| Decimal::from_scientific(&raw))
        .map_err(|error| {
            GatewayError::FuturesPreflight(format!("{context} response has invalid {key}: {error}"))
        })
}

pub(super) fn required_futures_bool(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<bool, GatewayError> {
    body.get(key)
        .and_then(|value| value.as_bool())
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

pub(super) fn required_futures_bool_or_string(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<bool, GatewayError> {
    body.get(key)
        .and_then(|value| {
            value
                .as_bool()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<bool>().ok()))
        })
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

pub(super) fn required_futures_u32(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<u32, GatewayError> {
    body.get(key)
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        })
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

pub(super) fn required_futures_timestamp(
    body: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<u64, GatewayError> {
    body.get(key)
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        })
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

pub(super) fn required_futures_string<'a>(
    body: &'a serde_json::Value,
    key: &str,
    context: &str,
) -> Result<&'a str, GatewayError> {
    body.get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

pub(super) fn required_futures_array<'a>(
    body: &'a serde_json::Value,
    key: &str,
    context: &str,
) -> Result<&'a Vec<serde_json::Value>, GatewayError> {
    body.get(key)
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            GatewayError::FuturesPreflight(format!(
                "{context} response is missing or has malformed {key}"
            ))
        })
}

pub(super) fn matching_futures_symbol<'a>(
    items: &'a [serde_json::Value],
    symbol: &str,
    context: &str,
) -> Result<&'a serde_json::Value, GatewayError> {
    let mut matches = items
        .iter()
        .filter(|item| item.get("symbol").and_then(|value| value.as_str()) == Some(symbol));
    let item = matches.next().ok_or_else(|| {
        GatewayError::FuturesPreflight(format!("{context} response is missing {symbol}"))
    })?;
    if matches.next().is_some() {
        return Err(GatewayError::FuturesPreflight(format!(
            "{context} response contains duplicate entries for {symbol}"
        )));
    }
    Ok(item)
}
