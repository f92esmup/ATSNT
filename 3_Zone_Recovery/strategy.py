"""
Aquí definimos la estrategia que va a usar nuestro engine. Es el equivalente al script de MQL5.
"""
from nautilus_trader.trading.strategy import Strategy
from nautilus_trader.config import StrategyConfig


class ZoneRecoveryRSI(StrategyConfig):
    """
    Al escribir instrument_id: InstrumentId sin un =, no estás creando una variable de clase 
    tradicional (como sería x = 10). Estás declarando que cualquier instancia de esta 
    clase DEBE tener ese atributo y como no tiene un valor por defecto, es obligatoria 
    incluirlo.

    Vamos lo que nos ahorra es explicitar los self. en el __init__() de Strategy.
    """

class ZoneRecoveryRSI(Strategy):

    def __init__(self, config:StrategyConfig) -> None:
        """
        Siguiendo las buenas practicas, los indicadores se crean aquí
        y se incluyen en la estrategia en el on_start.
        """
        super().__init__(config)

        # Aquí construyo los indicadores u otras cosas, pero los PARÁMETROS 
        # los establezco en la configuración exterior.

    
    def on_start(self)-> None:
        """
        Siguiendo la recomendación, aquí nos suscribimos 
        a los datos y añadimos indicadores a nuestra estrategia.
        """