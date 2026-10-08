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
            'metric.position': 'Posición Activa',
            'metric.drawdown': 'Drawdown de Sesión',
            'metric.uptime': 'Tiempo Activo',
            'metric.clients': 'Clientes Conectados',
            'metric.last_price': 'Último Precio',

            // Status & Alerts
            'status.loading': 'Cargando datos...',
            'status.no_data': 'Serie no disponible',
            'status.read_only': 'Modo Solo Lectura'
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
            'metric.position': 'Active Position',
            'metric.drawdown': 'Session Drawdown',
            'metric.uptime': 'Server Uptime',
            'metric.clients': 'Connected Clients',
            'metric.last_price': 'Last Price',

            // Status & Alerts
            'status.loading': 'Loading data...',
            'status.no_data': 'Series not available',
            'status.read_only': 'Read-Only Mode'
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
