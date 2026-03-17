"""
Aquí definimos la estrategia que va a usar nuestro engine. Es el equivalente al script de MQL5.
"""

import datetime as dt

from nautilus_trader.trading.strategy import Strategy
from nautilus_trader.config import StrategyConfig
from nautilus_trader.indicators import RelativeStrengthIndex
from nautilus_trader.indicators import MovingAverageType
from nautilus_trader.model.identifiers import InstrumentId
class ZoneRecoveryRSI(StrategyConfig):
    """
    Al escribir instrument_id: InstrumentId sin un =, no estás creando una variable de clase 
    tradicional (como sería x = 10). Estás declarando que cualquier instancia de esta 
    clase DEBE tener ese atributo y como no tiene un valor por defecto, es obligatoria 
    incluirlo.

    Vamos lo que nos ahorra es explicitar los self. en el __init__() de Strategy.
    """
    instrument_id: InstrumentId

    # respecto al RSI
    periodo_rsi: int
    zona_sobrecompra: int
    zona_sobreventa: int



class ZoneRecoveryRSI(Strategy):

    def __init__(self, config:StrategyConfig) -> None:
        """
        Siguiendo las buenas practicas, los indicadores se crean aquí
        y se incluyen en la estrategia en el on_start.
        """
        super().__init__(config)

        # Aquí construyo los indicadores u otras cosas, pero los PARÁMETROS 
        # los establezco en la configuración exterior.
        self.instrument_id = config.instrument_id
        # Creamos el indicador del RSI
        self.indicador_RSI = RelativeStrengthIndex.create(config.periodo_rsi, MovingAverageType.WILDER)

        # Timestamp para evaluar la 
        self.start_time = None
        self.end_time = None

    def on_start(self)-> None:
        """
        Siguiendo la recomendación, aquí nos suscribimos 
        a los datos y añadimos indicadores a nuestra estrategia.
        """

        ## Iniciamos el instante de inicio
        self.start_time = dt.datetime.now()
        self.log.info(f"La estrategia comenzó en el tiempo: {self.start_time}")

        # Recuperamos el instrumento de la cache:
        self.instrument = self.cache.instrument(self.instrument_id)

        if self.instrument is None:
            self.log.error(f"No se pudo encontrar un instrumento para {self.instrument_id}")
            self.stop() # Transción al estado STOPPED de la estrategia.
            return
        
        self.log.info(f"Se ha recuperado de la cache el instrumento: {self.instrument_id}")

        # Detalle importante aqui sobre la solicitud de datos: