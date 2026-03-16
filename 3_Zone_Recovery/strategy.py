"""
Aquí definimos la estrategia que va a usar nuestro engine. Es el equivalente al script de MQL5.
"""
from nautilus_trader.trading.strategy import Strategy

class ZoneRecoveyRsi(Strategy):

    def __init__(self):
        super().__init__()