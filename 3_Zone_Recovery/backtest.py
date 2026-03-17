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
from nautilus_trader.model.data import BarType
from nautilus_trader.persistence.wranglers import BarDataWrangler

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
    #Usamos el DataCatalog creado en el script datacatalog.py
#--------------------------------------------------------------------------------------
#--------------------------------------------------------------------------------------
    # PASO 1: Creamos la parte del DataLoader
    # 1.1 Cargamos y concatenamos múltiples archivos
    files = [r'data\raw\BTCUSDT-30m-2026-01.csv', r'data\raw\BTCUSDT-30m-2026-02.csv']
    # Cargamos cada CSV en una lista de DataFrames
    df_list = [pd.read_csv(f, header=0) for f in files]
    # Concatenamos todos en un único DataFrame
    df = pd.concat(df_list, ignore_index=True)

    # 1.2 Limpieza y Estructura de Tiempo
    # Convertimos a datetime
    df['timestamp'] = pd.to_datetime(df['open_time'], unit='ms')
    df.set_index('timestamp', inplace=True)

    # ELIMINAR DUPLICADOS: Si un archivo termina donde empieza el otro, evitamos solapamientos.
    df = df[~df.index.duplicated(keep='first')]

    # ORDENAR: Crucial para comprobar gaps después
    df.sort_index(inplace=True)

    # 1.3 Comprobación de Gaps (Continuidad)
    # Calculamos la diferencia entre cada timestamp. Para 30m, esperamos 30 min.
    expected_delta = pd.Timedelta(minutes=30)
    gaps = df.index.to_series().diff()[1:] # Ignoramos la primera fila
    missing_data = gaps[gaps != expected_delta]

    if not missing_data.empty:
        print(f"¡Cuidado! Se detectaron gaps en: {missing_data.index.tolist()}")
    else:
        print("Continuidad verificada: No hay saltos en el tiempo.")

    # 1.4 Seleccionar solo las 5 columnas requeridas y asegurar tipos flotantes.
    # Nota: Doble corchete [[...]] para que Pandas devuelva un DataFrame, no una Serie.
    df = df[['open', 'high', 'low', 'close', 'volume']].astype(float)
    print(df.head())

    # 1.5 Creamos el BarType (Asegúrate de que coincida con tu InstrumentId)
    barras = BarType.from_str("BTCUSDT.BINANCE-30-MINUTE-LAST-EXTERNAL")

    # PASO 2: DataWrangler
    # Ahora el df está limpio, continuo y con el formato perfecto.
    wrangler = BarDataWrangler(barras, instrument)
    bar_list = wrangler.process(df)
#--------------------------------------------------------------------------------------
#--------------------------------------------------------------------------------------
    # Lo añadimos al engine
    engine.add_data(bar_list)

    # PASO 5: Crear una estrategia y añadirla al engine
    

    # PASO 6: Reports y visualización del tearsheets
    engine.trader.generate_order_fills_report()
    
if __name__ == '__main__':
    main()
