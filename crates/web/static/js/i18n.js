// ATSNT Read-Only Web Workspace — Internationalization (i18n) Module (W1)
(function (global) {
    'use strict';

    const STORAGE_KEY = 'atsnt_locale';
    const DEFAULT_LOCALE = 'es';

    const translations = {
        es: {
            // Products
            'product.portal': 'Portal',
            'product.trading': 'Trading Workspace',
            'product.econoweb': 'Econoweb',
            'product.external': 'Externo',

            // Modules
            'module.overview': 'Resumen Global',
            'module.operations': 'Operaciones',
            'module.laboratory': 'Laboratorio',
            'module.strategies': 'Estrategias',
            'module.data': 'Datos y Mercado',
            'module.system': 'Sistema y Red',

            // Navigation & Header
            'nav.title': 'Espacio de Trading',
            'header.context': 'Contexto:',
            'wallet.spot': 'Binance Spot',
            'wallet.futures': 'USDⓈ-M Futuros (USDT)',

            // Modes & Badges
            'badge.demo': 'DEMO',
            'badge.paper': 'PAPER TRADING',
            'badge.live': 'PRODUCCIÓN',
            'badge.stale': 'DATOS DESACTUALIZADOS',
            'badge.connected': 'CONECTADO',
            'badge.disconnected': 'DESCONECTADO',

            // Clock & Time
            'clock.utc': 'UTC',
            'clock.local': 'LOCAL',

            // Metric Labels
            'metric.equity': 'Valor del Portafolio',
            'metric.cash': 'Saldo en Efectivo',
            'metric.upnl': 'PnL No Realizado',
            'metric.rpnl': 'PnL Realizado (Sesión)',
            'metric.position': 'Posición Activa',
            'metric.drawdown': 'Drawdown de Sesión',
            'metric.capital_at_risk': 'Capital en Riesgo',
            'metric.margin': 'Margen Aislado',
            'metric.leverage': 'Apalancamiento',
            'metric.funding': 'Tasa Financiación (8h)',
            'metric.uptime': 'Tiempo Activo',
            'metric.clients': 'Clientes Conectados',
            'metric.last_price': 'Último Precio',
            'metric.not_applicable': 'N/A (Spot Cash)',

            // Policies & Limits
            'policy.title': 'Políticas de Riesgo de Sesión',
            'policy.order_cap': 'Máx Notional/Orden: 10,000 USDT',
            'policy.pos_cap': 'Máx Notional/Posición: 50,000 USDT',
            'policy.risk_per_trade': 'Riesgo Máx/Trade: 1.00%',
            'policy.dd_limit': 'Límite DD Sesión: 5.00% HWM',
            'policy.status_normal': 'LÍMITES ACTIVOS',
            'policy.status_tripped': 'CIRCUIT BREAKER ACTIVADO',

            // Tabs
            'tab.positions': 'Posición Activa',
            'tab.orders': 'Órdenes Vivas',
            'tab.fills': 'Fills / Ejecuciones',
            'tab.closed_trades': 'Operaciones Cerradas',
            'tab.journal': 'Bitácora de Telemetría',

            // Time Filters
            'filter.all': 'Todo',
            'filter.1h': '1h',
            'filter.24h': '24h',
            'filter.session': 'Sesión',

            // Status & Alerts
            'status.loading': 'Cargando datos...',
            'status.no_data': 'Serie no disponible',
            'status.read_only': 'Modo Solo Lectura',
            'banner.demo_warning': 'MODO DEMO / SIMULACIÓN — Datos sintéticos generados en memoria. No representan saldos reales ni órdenes en exchange.',
            'strategy.incompatible_spot': 'Incompatible con Spot (Requiere USDⓈ-M Futuros)',
            'strategy.compatible_futures': 'Compatible con USDⓈ-M Futuros'
        },
        en: {
            // Products
            'product.portal': 'Portal',
            'product.trading': 'Trading Workspace',
            'product.econoweb': 'Econoweb',
            'product.external': 'External',

            // Modules
            'module.overview': 'Overview',
            'module.operations': 'Operations',
            'module.laboratory': 'Laboratory',
            'module.strategies': 'Strategies',
            'module.data': 'Data & Market',
            'module.system': 'System & Health',

            // Navigation & Header
            'nav.title': 'Trading Workspace',
            'header.context': 'Context:',
            'wallet.spot': 'Binance Spot',
            'wallet.futures': 'USDⓈ-M Futures (USDT)',

            // Modes & Badges
            'badge.demo': 'DEMO',
            'badge.paper': 'PAPER TRADING',
            'badge.live': 'LIVE',
            'badge.stale': 'STALE DATA',
            'badge.connected': 'CONNECTED',
            'badge.disconnected': 'DISCONNECTED',

            // Clock & Time
            'clock.utc': 'UTC',
            'clock.local': 'LOCAL',

            // Metric Labels
            'metric.equity': 'Portfolio Value',
            'metric.cash': 'Cash Balance',
            'metric.upnl': 'Unrealized PnL',
            'metric.rpnl': 'Realized PnL (Session)',
            'metric.position': 'Active Position',
            'metric.drawdown': 'Session Drawdown',
            'metric.capital_at_risk': 'Capital at Risk',
            'metric.margin': 'Isolated Margin',
            'metric.leverage': 'Effective Leverage',
            'metric.funding': 'Funding Rate (8h)',
            'metric.uptime': 'Server Uptime',
            'metric.clients': 'Connected Clients',
            'metric.last_price': 'Last Price',
            'metric.not_applicable': 'N/A (Spot Cash)',

            // Policies & Limits
            'policy.title': 'Session Risk Policies',
            'policy.order_cap': 'Max Notional/Order: 10,000 USDT',
            'policy.pos_cap': 'Max Notional/Position: 50,000 USDT',
            'policy.risk_per_trade': 'Max Risk/Trade: 1.00%',
            'policy.dd_limit': 'Session DD Limit: 5.00% HWM',
            'policy.status_normal': 'LIMITS ACTIVE',
            'policy.status_tripped': 'CIRCUIT BREAKER TRIPPED',

            // Tabs
            'tab.positions': 'Active Position',
            'tab.orders': 'Live Orders',
            'tab.fills': 'Fills & Executions',
            'tab.closed_trades': 'Closed Trades',
            'tab.journal': 'Telemetry Journal',

            // Time Filters
            'filter.all': 'All',
            'filter.1h': '1h',
            'filter.24h': '24h',
            'filter.session': 'Session',

            // Status & Alerts
            'status.loading': 'Loading data...',
            'status.no_data': 'Series not available',
            'status.read_only': 'Read-Only Mode',
            'banner.demo_warning': 'DEMO / SIMULATION MODE — In-memory synthetic data. Does not represent real balances or exchange executions.',
            'strategy.incompatible_spot': 'Incompatible with Spot (Requires USDⓈ-M Futures)',
            'strategy.compatible_futures': 'Compatible with USDⓈ-M Futures'
        }
    };

    let currentLocale = localStorage.getItem(STORAGE_KEY) || DEFAULT_LOCALE;
    if (!translations[currentLocale]) {
        currentLocale = DEFAULT_LOCALE;
    }

    function t(key) {
        const dict = translations[currentLocale] || translations[DEFAULT_LOCALE];
        return dict[key] || key;
    }

    function getLocale() {
        return currentLocale;
    }

    function setLocale(newLocale) {
        if (translations[newLocale]) {
            currentLocale = newLocale;
            localStorage.setItem(STORAGE_KEY, newLocale);
            applyTranslations();
            // Dispatch event for other components
            window.dispatchEvent(new CustomEvent('atsnt:locale-changed', { detail: { locale: newLocale } }));
        }
    }

    function applyTranslations() {
        document.querySelectorAll('[data-i18n]').forEach(el => {
            const key = el.getAttribute('data-i18n');
            if (key) {
                el.textContent = t(key);
            }
        });
        document.querySelectorAll('[data-i18n-title]').forEach(el => {
            const key = el.getAttribute('data-i18n-title');
            if (key) {
                el.setAttribute('title', t(key));
            }
        });
        document.querySelectorAll('[data-i18n-tooltip]').forEach(el => {
            const key = el.getAttribute('data-i18n-tooltip');
            if (key) {
                el.setAttribute('data-tooltip', t(key));
            }
        });
    }

    global.I18n = {
        t,
        getLocale,
        setLocale,
        applyTranslations
    };
})(window);
