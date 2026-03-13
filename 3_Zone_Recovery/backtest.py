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
        starting_balances = [Money(10_000,EUR)], #Cuenta de 10k € 
        base_currency = EUR,   # La moneda en la que está la cuenta.
        default_leverage = Decimal(10), # Apalancamiento 10x
    )

    #PASO 3: Crear la definición de un instrumento e incluirlo al engine.

if __name__ == '__main__':
    main()
