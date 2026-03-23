"""
Aquí definimos la estrategia que va a usar nuestro engine. Es el equivalente al script de MQL5.
"""

import datetime as dt

from nautilus_trader.trading.strategy import Strategy
from nautilus_trader.config import StrategyConfig
from nautilus_trader.indicators import RelativeStrengthIndex
from nautilus_trader.indicators import MovingAverageType
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model import BarType
from nautilus_trader.model.data import Bar

class ZoneRecoveryRSIConfig(StrategyConfig):
    """
    Al escribir instrument_id: InstrumentId sin un =, no estás creando una variable de clase 
    tradicional (como sería x = 10). Estás declarando que cualquier instancia de esta 
    clase DEBE tener ese atributo y como no tiene un valor por defecto, es obligatoria 
    incluirlo.

    Vamos lo que nos ahorra es explicitar los self. en el __init__() de Strategy.
    """
    instrument_id: InstrumentId
    bar_type: BarType

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
        self.bar_type = config.bar_type
        # Creamos el indicador del RSI
        self.indicador_RSI = RelativeStrengthIndex(config.periodo_rsi, MovingAverageType.WILDER)

        self.sobrecompra = config.zona_sobrecompra
        self.sobreventa = config.zona_sobreventa

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
       # self.request_bars(self.bar_type)

        # nos suscribimos a lso datos.
        self.subscribe_bars(self.bar_type)
        self.log.info(f"Subscritos a {self.bar_type}")

        # Y después de susbcribirnos a las barras es cuando registramos el indicador:
        self.register_indicator_for_bars(self.bar_type, self.indicador_RSI)
        self.log.info("Incluido el RSI")
        self.log.info("Estrategia inicializada")

    
    def on_bar(self, bar: Bar):
        """Funcion que maneja el evento de la barra"""

        ## ALGO PASA CON ESTA LÓGICA.
    ## Para acceder al indicador simplemente se llama al objeto:
        if self.indicador_RSI.value >= float(self.sobrecompra):
            # El mercado está agotado al alza, se busca VENDER
            self.log.info(f"VENTA (Sobrecompra): {self.indicador_RSI.value}")

        elif self.indicador_RSI.value <= float(self.sobreventa):
            # El mercado está agotado a la baja, se busca COMPRAR
            self.log.info(f"COMPRA (Sobreventa): {self.indicador_RSI.value}")

        else:
            self.log.info(f"Neutral: {self.indicador_RSI.value}. No hay operación posible.")

