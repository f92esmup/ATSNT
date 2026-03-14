"""
Este script crea el DataCatalog que usaremos en esta estrategia.
"""
import pandas as pd
from nautilus_trader.persistence.catalog import ParquetDataCatalog
from nautilus_trader.model.data import BarType
from nautilus_trader.persistence.wranglers import BarDataWrangler
#Voy a incluir un logger.


def main() -> None:

    # Creo que la definición del instrumento la tengo que hacer aquí para añadirla 
    # al catalog. Pero luego debo añadirlo al engine tambien...
    


    # PASO 1: Instanciamos el DataCatalog
    # Uso r'' para evitar problemas con las barras invertidas en Windows
    catalog = ParquetDataCatalog(r'data\catalog')
    # No se usa hasta el final del proceso.

    # PASO 2: Creamos la parte del DataLoader
    # 2.1 Cargamos y concatenamos múltiples archivos
    files = [r'data\raw\BTCUSDT-30m-2026-01.csv', r'data\raw\BTCUSDT-30m-2026-02.csv']
    # Cargamos cada CSV en una lista de DataFrames
    df_list = [pd.read_csv(f, header=0) for f in files]
    # Concatenamos todos en un único DataFrame
    df = pd.concat(df_list, ignore_index=True)

    # 2.2 Limpieza y Estructura de Tiempo
    # Convertimos a datetime
    df['timestamp'] = pd.to_datetime(df['open_time'], unit='ms')
    df.set_index('timestamp', inplace=True)

    # ELIMINAR DUPLICADOS: Si un archivo termina donde empieza el otro, evitamos solapamientos.
    df = df[~df.index.duplicated(keep='first')]

    # ORDENAR: Crucial para comprobar gaps después
    df.sort_index(inplace=True)

    # 2.3 Comprobación de Gaps (Continuidad)
    # Calculamos la diferencia entre cada timestamp. Para 30m, esperamos 30 min.
    expected_delta = pd.Timedelta(minutes=30)
    gaps = df.index.to_series().diff()[1:] # Ignoramos la primera fila
    missing_data = gaps[gaps != expected_delta]

    if not missing_data.empty:
        print(f"¡Cuidado! Se detectaron gaps en: {missing_data.index.tolist()}")
    else:
        print("Continuidad verificada: No hay saltos en el tiempo.")

    # 2.4 Seleccionar solo las 5 columnas requeridas y asegurar tipos flotantes.
    # Nota: Doble corchete [[...]] para que Pandas devuelva un DataFrame, no una Serie.
    df = df[['open', 'high', 'low', 'close', 'volume']].astype(float)
    print(df.head())

    # 2.5 Creamos el BarType (Asegúrate de que coincida con tu InstrumentId)
    barras = BarType.from_str("BTCUSDT.BINANCE-30-MINUTE-LAST-EXTERNAL")

    # PASO 3: DataWrangler
    # Ahora el df está limpio, continuo y con el formato perfecto.
    wrangler = BarDataWrangler(barras, instrument)
    bar_list = wrangler.process(df)

    catalog.write_data(bar_list)

if __name__ == "__main__":
    main()