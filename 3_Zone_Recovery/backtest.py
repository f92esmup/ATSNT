"""
Script que realizar el backtest de strategy.
"""
from decimal import Decimal
import pandas as pd

from nautilus_trader.config import BacktestEngineConfig
from nautilus_trader.model import TraderId
from nautilus_trader.config import LoggingConfig
from nautilus_trader.backtest.engine import BacktestEngine
from nautilus_trader.model import Venue
from nautilus_trader.model.enums import OmsType
from nautilus_trader.model.enums import AccountType
from nautilus_trader.model.objects import Money
from nautilus_trader.model.currencies import EUR
from nautilus_trader.model.instruments import CryptoPerpetual
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.identifiers import Symbol
from nautilus_trader.model.currencies import BTC, USDT
from nautilus_trader.model import Price, Quantity

def main() -> None:

    # PASO 1: Creamos el engine de nuestra estrategia. Backtest en este caso
    # 1.1 creamos la configuración
    config_engine: BacktestEngineConfig = BacktestEngineConfig(
        trader_id= TraderId("ZONERSIBACKTEST-001"),
        logging=LoggingConfig(
            log_level="DEBUG",
        ), # Establece el logging de la terminal a debug.
    )

    # 1.2 instanciamos el engine
    engine: BacktestEngine = BacktestEngine(
        config=config_engine,
    )

    #PASO 2: Definimos el exchange que vamos a utlizar y lo añadimos al motor
    engine.add_venue(
        venue= Venue('BINANCE'), # Añadimos Binance como venue.
        oms_type= OmsType.NETTING, # Gestionamos una sola posición.
        account_type= AccountType.MARGIN, # Necesitamos margin porque vamos a operar perpetual futures.
        starting_balances = [Money(10_000,USDT)], #Cuenta de 10k € 
        base_currency = USDT,   # La moneda en la que está la cuenta.
        default_leverage = Decimal(10), # Apalancamiento 10x
    ) # NOTE: Ponemos USDT y no EUR PORQE VAMOS A OPERAR EN USDT y esa es la moneda que tendremos en BINANCE.

    #PASO 3: Crear la definición de un instrumento e incluirlo al engine.
    instrument = CryptoPerpetual(
        instrument_id = InstrumentId(Symbol('BTCUSDT'), Venue('BINANCE')), #Podría haber usado InstrumentID.from_str("BTCUSDT.BINANCE")
        raw_symbol = Symbol('BTCUSDT'),
        base_currency = BTC, #moneda que estamos comprando
        quote_currency = USDT, # contraparte del par
        settlement_currency = USDT, # moneda en la que recibimos los beneficios, correlacionado con is_inverse.
        is_inverse = False, # trading tradicional, pnl lineal
        price_precision = 2, #dos decimales de precisión en el precio "87608.30"
        size_precision = 3, # 0.001 BTC es lo mínimo que se puede comprar.
        price_increment = Price.from_str('0.01'), # El incremento mínimo en el preio (tick).
        size_increment = Quantity.from_str('0.001'), # Incremento del tamaño de la operación.
        ts_event = 0,
        ts_init = 0,
        lot_size = Quantity.from_str('0.001'),
        margin_init = Decimal(0.10),
        margin_maint = Decimal(0.05),
        maker_fee = Decimal(0.0002),
        taker_fee = Decimal(0.0004),
    )

    # Añadimos el instrumento al motor:
    engine.add_instrument(instrument)

    #PASO 4: Creamos nuestra fuente de datos a partir de un CSV.


if __name__ == '__main__':
    main()
