# Binance private-event fixtures

These compact, synthetic fixtures retain the field names and value types from Binance's official
event schemas. The numeric values and identifiers are non-secret test data; they are not captured
account records.

- Spot `outboundAccountPosition`, `balanceUpdate`, and `executionReport`: [Spot User Data Stream schemas](https://developers.binance.com/en/docs/products/spot/user-data-stream)
- USD-M `ACCOUNT_UPDATE`, `ORDER_TRADE_UPDATE`, and `listenKeyExpired`: [USDⓈ-M Futures User Data Streams](https://developers.binance.com/en/docs/catalog/core-trading-derivatives-trading-usd-m-futures/api/ws-api/user-data-streams)
- Spot signed subscription request schema: [Spot WebSocket API User Data Stream](https://developers.binance.com/en/docs/catalog/core-trading-spot-trading/api/ws-api/user-data-stream)
- Signed WebSocket parameter canonicalization: [Official Spot WebSocket API signing examples](https://github.com/binance/binance-spot-api-docs/blob/master/web-socket-api.md)

The Spot account-position fixture deliberately contains only one changed asset. It must not be
interpreted as a complete account snapshot. The balance-update fixture is a signed delta.
