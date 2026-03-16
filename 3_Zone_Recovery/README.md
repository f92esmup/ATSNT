# Estructura de archivos

backtest.py es el backtest de strategy.py usando el BacktestEngine. Esto es usando la api de bajo nivel.

backtestnode.py es el backtest de strategy.py usando el BacktestNode. Usando la api de alto nivel y separando los datos en el script datacatalog, donde genero todos los datos deseados en ese script y los guardo en el catalgo, luego el catalog se carga en el script principal de BacktestNode. 
