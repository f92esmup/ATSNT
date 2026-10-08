// ATSNT Quantitative Web Dashboard Controller — W3 Operations Center
(function () {
    'use strict';

    // State
    let socket = null;
    let reconnectDelay = 1000;
    let liveChart = null;
    let candleSeries = null;
    let backtestChart = null;
    let backtestSeries = null;
    let hpoChart = null;
    let chartMarkers = [];
    let telemetryStale = false;
    let executionMode = 'idle';
    let activeWalletContext = 'futures';
    let activeTimeFilter = 'all';

    // Session Data Buffers
    let sessionFills = [];
    let sessionClosedTrades = [];
    let sessionOrders = [];
    let sessionEvents = [];
    let sessionRealizedPnl = 0.0;
    let lastEventTimestamp = null;
    let lastBarTimestamp = null;
    let activePositionData = null;
    let currentMarkPrice = null;

    // DOM Elements Cache
    const wsStatus = document.getElementById('ws-status');
    const wsStatusText = document.getElementById('ws-status-text');
    const livePriceEl = document.getElementById('live-price');
    const demoModeBanner = document.getElementById('demo-mode-banner');
    const circuitBreakerStatus = document.getElementById('circuit-breaker-status');

    // KPI Elements
    const kpiEquity = document.getElementById('kpi-equity');
    const kpiCash = document.getElementById('kpi-cash');
    const kpiUpnl = document.getElementById('kpi-upnl');
    const kpiUpnlPct = document.getElementById('kpi-upnl-pct');
    const kpiRpnl = document.getElementById('kpi-rpnl');
    const kpiTradesSummary = document.getElementById('kpi-trades-summary');
    const kpiDrawdown = document.getElementById('kpi-drawdown');
    const kpiCapitalAtRisk = document.getElementById('kpi-capital-at-risk');
    const kpiCapitalAtRiskPct = document.getElementById('kpi-capital-at-risk-pct');
    const kpiMargin = document.getElementById('kpi-margin');
    const kpiLeverage = document.getElementById('kpi-leverage');
    const kpiFunding = document.getElementById('kpi-funding');
    const kpiFreshness = document.getElementById('kpi-freshness');
    const kpiClientsFresh = document.getElementById('kpi-clients-fresh');

    // Chart & Side Panel Elements
    const chartAssetTitle = document.getElementById('chart-asset-title');
    const barDuration = document.getElementById('bar-duration');
    const posBadge = document.getElementById('pos-badge');
    const posEntry = document.getElementById('pos-entry');
    const posMark = document.getElementById('pos-mark');
    const posQty = document.getElementById('pos-qty');
    const posSl = document.getElementById('pos-sl');
    const posTp = document.getElementById('pos-tp');
    const posUpnlDetail = document.getElementById('pos-upnl-detail');
    const engineModeVal = document.getElementById('engine-mode-val');
    const strategyCompatBadge = document.getElementById('strategy-compat-badge');

    // Operational Tables & Counters
    const positionsBody = document.getElementById('positions-body');
    const ordersBody = document.getElementById('orders-body');
    const fillsBody = document.getElementById('fills-body');
    const fillsCount = document.getElementById('fills-count');
    const tradesBody = document.getElementById('trades-body');
    const tradesCount = document.getElementById('trades-count');
    const eventsBody = document.getElementById('events-body');
    const eventsCount = document.getElementById('events-count');

    // 1. Initialize TradingView Candlestick Chart
    function initLiveChart() {
        const container = document.getElementById('chart-live');
        if (!container || typeof LightweightCharts === 'undefined') return;

        liveChart = LightweightCharts.createChart(container, {
            layout: {
                background: { color: '#111827' },
                textColor: '#94a3b8',
                fontFamily: 'JetBrains Mono, monospace',
            },
            grid: {
                vertLines: { color: '#1e293b' },
                horzLines: { color: '#1e293b' },
            },
            timeScale: {
                timeVisible: true,
                secondsVisible: true,
                borderColor: '#1e293b',
            },
            rightPriceScale: {
                borderColor: '#1e293b',
            },
            crosshair: {
                vertLine: { color: '#38bdf8', labelBackgroundColor: '#0284c7' },
                horzLine: { color: '#38bdf8', labelBackgroundColor: '#0284c7' },
            }
        });

        candleSeries = liveChart.addCandlestickSeries({
            upColor: '#10b981',
            downColor: '#f43f5e',
            borderUpColor: '#10b981',
            borderDownColor: '#f43f5e',
            wickUpColor: '#10b981',
            wickDownColor: '#f43f5e',
        });

        // Responsive resize
        const ro = new ResizeObserver(entries => {
            if (entries.length && liveChart) {
                liveChart.applyOptions({
                    width: entries[0].contentRect.width,
                    height: entries[0].contentRect.height,
                });
            }
        });
        ro.observe(container);
    }

    // 2. WebSocket Connection and Stream Management
    function connectWebSocket() {
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const wsUrl = `${protocol}//${window.location.host}/ws/telemetry`;

        if (wsStatus) {
            wsStatus.className = 'status-badge';
            if (wsStatusText) wsStatusText.textContent = 'CONNECTING...';
        }

        try {
            socket = new WebSocket(wsUrl);
        } catch (e) {
            scheduleReconnect();
            return;
        }

        socket.onopen = function () {
            showTelemetryStatus();
            reconnectDelay = 1000;
            appendLog('SYSTEM', '---', 'Conexión establecida con el flujo de telemetría');
        };

        socket.onmessage = function (event) {
            try {
                const data = JSON.parse(event.data);
                handleTelemetryEvent(data);
            } catch (err) {
                console.error('Failed parsing WS payload:', err);
            }
        };

        socket.onclose = function () {
            if (wsStatus) {
                wsStatus.className = 'status-badge';
                if (wsStatusText) wsStatusText.textContent = 'DISCONNECTED';
            }
            scheduleReconnect();
        };

        socket.onerror = function () {
            socket.close();
        };
    }

    function scheduleReconnect() {
        setTimeout(() => {
            reconnectDelay = Math.min(reconnectDelay * 1.5, 10000);
            connectWebSocket();
        }, reconnectDelay);
    }

    function showTelemetryStatus() {
        if (!wsStatus || !wsStatusText) return;
        wsStatus.className = telemetryStale ? 'status-badge badge-stale' : 'status-badge live';
        wsStatusText.textContent = telemetryStale ? 'STALE / UNCERTAIN' : 'LIVE WS';
    }

    // 3. Telemetry Event Processor
    function handleTelemetryEvent(msg) {
        lastEventTimestamp = Date.now();
        const type = msg.event_type;
        const p = msg.payload;
        const symbol = msg.symbol;
        const timestamp = msg.timestamp || Date.now();

        updateTelemetryIdentity(msg);

        if (type === 'InitialSnapshot') {
            updateInitialSnapshot(p);
            return;
        }

        if (type === 'BarFormed') {
            const bar = p.BarFormed || p;
            const candleTime = Math.floor(bar.end_time / 1000);
            const closeVal = parseFloat(bar.close);
            lastBarTimestamp = Date.now();

            // Calculate irregular duration Δt = end_time - start_time
            if (bar.start_time && bar.end_time) {
                const durationSecs = ((bar.end_time - bar.start_time) / 1000).toFixed(1);
                if (barDuration) barDuration.textContent = `${durationSecs}s`;
            }

            if (candleSeries) {
                candleSeries.update({
                    time: candleTime,
                    open: parseFloat(bar.open),
                    high: parseFloat(bar.high),
                    low: parseFloat(bar.low),
                    close: closeVal,
                });
            }

            if (livePriceEl) {
                livePriceEl.textContent = `$${closeVal.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
            }

            appendLog('BAR_FORMED', symbol, `$${closeVal.toFixed(2)}`,
                `Vol: ${parseFloat(bar.volume).toFixed(4)} ($${parseFloat(bar.dollar_volume).toFixed(0)})`, timestamp, msg.strategy_id);

        } else if (type === 'MarkToMarket') {
            const m = p.MarkToMarket || p;
            const price = parseFloat(m.current_price);
            const upnl = parseFloat(m.unrealized_pnl);
            const totalEq = parseFloat(m.total_equity);
            const dd = parseFloat(m.drawdown_pct);
            currentMarkPrice = price;

            if (livePriceEl) {
                livePriceEl.textContent = `$${price.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
            }
            if (posMark) posMark.textContent = `$${price.toFixed(2)}`;
            if (kpiEquity) {
                kpiEquity.textContent = `$${totalEq.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
            }

            // Drawdown evaluation (Session 5% risk limit)
            const ddPctVal = dd * 100;
            if (kpiDrawdown) {
                kpiDrawdown.textContent = `${ddPctVal.toFixed(2)}%`;
                kpiDrawdown.className = `metric-value ${ddPctVal >= 4.0 ? 'negative' : ddPctVal > 0 ? 'neutral' : 'positive'}`;
            }

            // Circuit breaker state check
            if (circuitBreakerStatus) {
                if (ddPctVal >= 5.0) {
                    circuitBreakerStatus.className = 'badge badge-stale';
                    circuitBreakerStatus.textContent = window.I18n ? window.I18n.t('policy.status_tripped') : 'CIRCUIT BREAKER ACTIVADO';
                } else {
                    circuitBreakerStatus.className = 'badge badge-live';
                    circuitBreakerStatus.textContent = window.I18n ? window.I18n.t('policy.status_normal') : 'LÍMITES ACTIVOS';
                }
            }

            if (kpiUpnl) {
                kpiUpnl.textContent = `${upnl >= 0 ? '+' : ''}$${upnl.toFixed(2)}`;
                kpiUpnl.className = `metric-value ${upnl > 0 ? 'positive' : upnl < 0 ? 'negative' : 'neutral'}`;
            }

            if (kpiUpnlPct) {
                const upnlPct = totalEq > 0 ? (upnl / totalEq) * 100 : 0;
                kpiUpnlPct.textContent = `${upnlPct >= 0 ? '+' : ''}${upnlPct.toFixed(2)}%`;
                kpiUpnlPct.className = `metric-subtext ${upnl > 0 ? 'positive' : upnl < 0 ? 'negative' : 'neutral'}`;
            }

            if (posUpnlDetail) {
                posUpnlDetail.textContent = `${upnl >= 0 ? '+' : ''}$${upnl.toFixed(2)}`;
                posUpnlDetail.className = `detail-val ${upnl > 0 ? 'positive' : upnl < 0 ? 'negative' : 'neutral'}`;
            }

            if (activePositionData) {
                renderPositionsTable(activePositionData, price, upnl);
            }

        } else if (type === 'PositionOpened') {
            const pos = p.PositionOpened || p;
            const side = pos.side;
            const entry = parseFloat(pos.entry_price);
            const qty = parseFloat(pos.quantity);
            const sl = parseFloat(pos.stop_loss);
            const tp = parseFloat(pos.take_profit);
            activePositionData = pos;

            if (posBadge) {
                posBadge.className = `position-badge ${side.toLowerCase()}`;
                posBadge.textContent = side.toUpperCase();
            }
            if (posEntry) posEntry.textContent = `$${entry.toFixed(2)}`;
            if (posQty) posQty.textContent = `${qty.toFixed(4)} BTC`;
            if (posSl) posSl.textContent = `$${sl.toFixed(2)}`;
            if (posTp) posTp.textContent = `$${tp.toFixed(2)}`;

            // Capital at Risk: |Entry - SL| * Qty
            const riskAmount = Math.abs(entry - sl) * qty;
            if (kpiCapitalAtRisk) {
                kpiCapitalAtRisk.textContent = `$${riskAmount.toFixed(2)}`;
                kpiCapitalAtRisk.className = 'metric-value negative';
            }
            if (kpiCapitalAtRiskPct) {
                kpiCapitalAtRiskPct.textContent = `SL a $${sl.toFixed(2)}`;
            }

            // Derivatives Margin calculation (USD-M Futures)
            updateDerivativesMargin(entry, qty);

            // Record Fill
            const fillId = `fill-${timestamp}`;
            const fillSide = side.toLowerCase() === 'long' ? 'BUY' : 'SELL';
            sessionFills.unshift({
                id: fillId,
                timestamp,
                symbol,
                side: fillSide,
                price: entry,
                quantity: qty,
                fee: 0.0,
                execution_type: 'SIMULATED'
            });
            renderFillsTable();

            // Record Live Attached Orders (SL and TP)
            sessionOrders = [
                { id: `ord-sl-${timestamp}`, symbol, type: 'STOP_LOSS', side: fillSide === 'BUY' ? 'SELL' : 'BUY', quantity: qty, price: sl, status: 'NEW', timestamp },
                { id: `ord-tp-${timestamp}`, symbol, type: 'TAKE_PROFIT', side: fillSide === 'BUY' ? 'SELL' : 'BUY', quantity: qty, price: tp, status: 'NEW', timestamp }
            ];
            renderOrdersTable();
            renderPositionsTable(pos, entry, 0.0);

            // Chart Marker
            if (candleSeries) {
                chartMarkers.push({
                    time: Math.floor(timestamp / 1000),
                    position: side.toLowerCase() === 'long' ? 'belowBar' : 'aboveBar',
                    color: side.toLowerCase() === 'long' ? '#10b981' : '#f43f5e',
                    shape: side.toLowerCase() === 'long' ? 'arrowUp' : 'arrowDown',
                    text: `ENTRY ${side.toUpperCase()} @ ${entry.toFixed(0)}`,
                });
                candleSeries.setMarkers(chartMarkers);
            }

            appendLog('POSITION_OPEN', symbol, `$${entry.toFixed(2)}`,
                `${side.toUpperCase()} ${qty.toFixed(4)} (SL: ${sl.toFixed(0)}, TP: ${tp.toFixed(0)})`, timestamp, msg.strategy_id);

        } else if (type === 'PositionClosed') {
            const exit = p.PositionClosed || p;
            const pnl = parseFloat(exit.net_pnl);
            const exitPrice = parseFloat(exit.exit_price);
            const totalEq = parseFloat(exit.total_equity);

            // Record closing fill
            const fillId = `fill-${timestamp}`;
            const closeSide = activePositionData && activePositionData.side === 'Long' ? 'SELL' : 'BUY';
            const closeQty = activePositionData ? parseFloat(activePositionData.quantity) : 0.0;
            const entryPriceVal = activePositionData ? parseFloat(activePositionData.entry_price) : exitPrice;

            sessionFills.unshift({
                id: fillId,
                timestamp,
                symbol,
                side: closeSide,
                price: exitPrice,
                quantity: closeQty,
                fee: 0.0,
                execution_type: 'SIMULATED'
            });
            renderFillsTable();

            // Record Closed Trade
            sessionClosedTrades.unshift({
                id: `trade-${timestamp}`,
                timestamp,
                symbol,
                side: activePositionData ? activePositionData.side : 'Long',
                entry_price: entryPriceVal,
                exit_price: exitPrice,
                quantity: closeQty,
                net_pnl: pnl,
                exit_reason: exit.exit_reason || 'Manual'
            });
            sessionRealizedPnl += pnl;
            renderTradesTable();
            sessionOrders = [];
            renderOrdersTable();

            // Reset Position State
            activePositionData = null;
            if (posBadge) {
                posBadge.className = 'position-badge flat';
                posBadge.textContent = 'FLAT';
            }
            if (posEntry) posEntry.textContent = '---';
            if (posMark) posMark.textContent = '---';
            if (posQty) posQty.textContent = '---';
            if (posSl) posSl.textContent = '---';
            if (posTp) posTp.textContent = '---';
            if (posUpnlDetail) {
                posUpnlDetail.textContent = '$0.00';
                posUpnlDetail.className = 'detail-val neutral';
            }

            if (kpiEquity) {
                kpiEquity.textContent = `$${totalEq.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
            }
            if (kpiCash) {
                kpiCash.textContent = `Cash: $${totalEq.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
            }
            if (kpiUpnl) {
                kpiUpnl.textContent = '$0.00';
                kpiUpnl.className = 'metric-value neutral';
            }
            if (kpiUpnlPct) {
                kpiUpnlPct.textContent = '+0.00%';
                kpiUpnlPct.className = 'metric-subtext neutral';
            }
            if (kpiRpnl) {
                kpiRpnl.textContent = `${sessionRealizedPnl >= 0 ? '+' : ''}$${sessionRealizedPnl.toFixed(2)}`;
                kpiRpnl.className = `metric-value ${sessionRealizedPnl > 0 ? 'positive' : sessionRealizedPnl < 0 ? 'negative' : 'neutral'}`;
            }
            if (kpiCapitalAtRisk) {
                kpiCapitalAtRisk.textContent = '$0.00';
                kpiCapitalAtRisk.className = 'metric-value neutral';
            }
            if (kpiCapitalAtRiskPct) {
                kpiCapitalAtRiskPct.textContent = `Máx 1% ($${(totalEq * 0.01).toFixed(2)})`;
            }

            updateDerivativesMargin(0, 0);
            renderPositionsTable(null, exitPrice, 0.0);

            // Chart Marker
            if (candleSeries) {
                chartMarkers.push({
                    time: Math.floor(timestamp / 1000),
                    position: 'aboveBar',
                    color: pnl >= 0 ? '#10b981' : '#f43f5e',
                    shape: 'circle',
                    text: `EXIT (${exit.exit_reason}) ${pnl >= 0 ? '+' : ''}$${pnl.toFixed(2)}`,
                });
                candleSeries.setMarkers(chartMarkers);
            }

            appendLog('POSITION_CLOSED', symbol, `$${exitPrice.toFixed(2)}`,
                `Motivo: ${exit.exit_reason}, Net PnL: ${pnl >= 0 ? '+' : ''}$${pnl.toFixed(2)}`, timestamp, msg.strategy_id);

        } else if (type === 'SignalGenerated') {
            const s = p.SignalGenerated || p;
            const signalSide = s.side ? (typeof s.side === 'object' ? Object.keys(s.side)[0] : s.side) : 'Buy';
            const signalPrice = parseFloat(s.price || 0);

            if (candleSeries) {
                chartMarkers.push({
                    time: Math.floor(timestamp / 1000),
                    position: signalSide.toLowerCase() === 'buy' ? 'belowBar' : 'aboveBar',
                    color: '#38bdf8',
                    shape: signalSide.toLowerCase() === 'buy' ? 'arrowUp' : 'arrowDown',
                    text: `SIGNAL: ${signalSide.toUpperCase()}`,
                });
                candleSeries.setMarkers(chartMarkers);
            }

            appendLog('SIGNAL', symbol, `$${signalPrice.toFixed(2)}`, `Señal CUSUM: ${signalSide.toUpperCase()}`, timestamp, msg.strategy_id);
        }
    }

    // 4. Initial Snapshot Handler
    function updateInitialSnapshot(st) {
        if (!st) return;
        telemetryStale = telemetryStale || st.stale === true;
        showTelemetryStatus();

        if (st.execution_mode) {
            executionMode = st.execution_mode;
            applyExecutionMode(executionMode);
        }

        updateTelemetryIdentity({ symbol: st.active_symbol, strategy_id: st.active_strategy });

        const equity = parseFloat(st.portfolio_value);
        const upnl = parseFloat(st.unrealized_pnl);
        const cash = parseFloat(st.cash_balance);
        const dd = parseFloat(st.drawdown_pct);

        if (kpiEquity) {
            kpiEquity.textContent = `$${equity.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
        }
        if (kpiCash) {
            kpiCash.textContent = `Cash: $${cash.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
        }
        if (kpiUpnl) {
            kpiUpnl.textContent = `${upnl >= 0 ? '+' : ''}$${upnl.toFixed(2)}`;
            kpiUpnl.className = `metric-value ${upnl > 0 ? 'positive' : upnl < 0 ? 'negative' : 'neutral'}`;
        }
        if (kpiUpnlPct) {
            const pct = equity > 0 ? (upnl / equity) * 100 : 0;
            kpiUpnlPct.textContent = `${pct >= 0 ? '+' : ''}${pct.toFixed(2)}%`;
            kpiUpnlPct.className = `metric-subtext ${upnl > 0 ? 'positive' : upnl < 0 ? 'negative' : 'neutral'}`;
        }
        if (kpiDrawdown) {
            const ddPct = (dd * 100).toFixed(2);
            kpiDrawdown.textContent = `${ddPct}%`;
        }

        const mark = st.last_price === null ? null : parseFloat(st.last_price);
        currentMarkPrice = mark;
        if (livePriceEl) {
            livePriceEl.textContent = mark === null ? '---' : `$${mark.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
        }

        // Hydrate recent session records if present in snapshot
        if (Array.isArray(st.recent_fills) && st.recent_fills.length > 0) {
            sessionFills = st.recent_fills.map(f => ({
                id: f.id,
                timestamp: f.timestamp,
                symbol: f.symbol,
                side: f.side,
                price: parseFloat(f.price),
                quantity: parseFloat(f.quantity),
                fee: parseFloat(f.fee || 0),
                execution_type: f.execution_type || 'SIMULATED'
            }));
            renderFillsTable();
        }

        if (Array.isArray(st.recent_closed_trades) && st.recent_closed_trades.length > 0) {
            sessionClosedTrades = st.recent_closed_trades.map(t => ({
                id: t.id,
                timestamp: t.timestamp,
                symbol: t.symbol,
                side: t.side,
                entry_price: parseFloat(t.entry_price),
                exit_price: parseFloat(t.exit_price),
                quantity: parseFloat(t.quantity),
                net_pnl: parseFloat(t.net_pnl),
                exit_reason: t.exit_reason
            }));
            sessionRealizedPnl = sessionClosedTrades.reduce((acc, trade) => acc + trade.net_pnl, 0);
            if (kpiRpnl) {
                kpiRpnl.textContent = `${sessionRealizedPnl >= 0 ? '+' : ''}$${sessionRealizedPnl.toFixed(2)}`;
                kpiRpnl.className = `metric-value ${sessionRealizedPnl > 0 ? 'positive' : sessionRealizedPnl < 0 ? 'negative' : 'neutral'}`;
            }
            renderTradesTable();
        }

        const pos = st.active_position;
        if (pos) {
            activePositionData = pos;
            const side = pos.side;
            const entry = parseFloat(pos.entry_price);
            const qty = parseFloat(pos.quantity);
            const sl = parseFloat(pos.stop_loss);
            const tp = parseFloat(pos.take_profit);

            if (posBadge) {
                posBadge.className = `position-badge ${side.toLowerCase()}`;
                posBadge.textContent = side.toUpperCase();
            }
            if (posEntry) posEntry.textContent = `$${entry.toFixed(2)}`;
            if (posQty) posQty.textContent = `${qty.toFixed(4)} BTC`;
            if (posSl) posSl.textContent = `$${sl.toFixed(2)}`;
            if (posTp) posTp.textContent = `$${tp.toFixed(2)}`;
            if (posMark) posMark.textContent = mark === null ? '---' : `$${mark.toFixed(2)}`;

            const riskAmount = Math.abs(entry - sl) * qty;
            if (kpiCapitalAtRisk) {
                kpiCapitalAtRisk.textContent = `$${riskAmount.toFixed(2)}`;
                kpiCapitalAtRisk.className = 'metric-value negative';
            }
            updateDerivativesMargin(entry, qty);
            renderPositionsTable(pos, mark || entry, upnl);
        } else {
            renderPositionsTable(null, mark || 0, 0);
            updateDerivativesMargin(0, 0);
        }
    }

    function applyExecutionMode(mode) {
        if (demoModeBanner) {
            demoModeBanner.style.display = mode === 'mock' ? 'flex' : 'none';
        }
        if (engineModeVal) {
            if (mode === 'mock') {
                engineModeVal.textContent = 'DEMO / MOCK';
                engineModeVal.style.color = 'var(--warning-amber)';
            } else if (mode === 'paper') {
                engineModeVal.textContent = 'PAPER TRADING';
                engineModeVal.style.color = 'var(--accent-green)';
            } else {
                engineModeVal.textContent = 'IDLE';
                engineModeVal.style.color = 'var(--text-muted)';
            }
        }
    }

    // 5. Derivatives Margin & Wallet Context Switcher
    function updateDerivativesMargin(entry, qty) {
        if (activeWalletContext === 'spot') {
            if (kpiMargin) {
                kpiMargin.textContent = 'N/A';
                kpiMargin.className = 'metric-value neutral';
            }
            if (kpiLeverage) kpiLeverage.textContent = 'Spot Cash (Sin Margen)';
            if (kpiFunding) kpiFunding.textContent = 'N/A';
            if (strategyCompatBadge) {
                strategyCompatBadge.className = 'badge badge-demo';
                strategyCompatBadge.textContent = window.I18n ? window.I18n.t('strategy.incompatible_spot') : 'Incompatible con Spot (Requiere Futuros)';
            }
        } else {
            // Futures isolated margin
            const notional = entry * qty;
            if (kpiMargin) {
                kpiMargin.textContent = `$${notional.toFixed(2)}`;
                kpiMargin.className = `metric-value ${notional > 0 ? 'positive' : 'neutral'}`;
            }
            if (kpiLeverage) kpiLeverage.textContent = '1x (Aislado USDT)';
            if (kpiFunding) kpiFunding.textContent = '+0.0100% (8h)';
            if (strategyCompatBadge) {
                strategyCompatBadge.className = 'badge badge-paper';
                strategyCompatBadge.textContent = window.I18n ? window.I18n.t('strategy.compatible_futures') : 'Compatible con USDⓈ-M Futuros';
            }
        }
    }

    function updateWalletContextView() {
        const walletSelect = document.getElementById('wallet-select');
        if (walletSelect) {
            activeWalletContext = walletSelect.value;
        }
        if (activePositionData) {
            const entry = parseFloat(activePositionData.entry_price);
            const qty = parseFloat(activePositionData.quantity);
            updateDerivativesMargin(entry, qty);
        } else {
            updateDerivativesMargin(0, 0);
        }
        if (chartAssetTitle) {
            chartAssetTitle.textContent = activeWalletContext === 'spot' ? 'BTCUSDT (SPOT DOLLAR BARS)' : 'BTCUSDT (PERP DOLLAR BARS)';
        }
    }

    // Listen to wallet changes dispatched by shell.js
    window.addEventListener('atsnt:wallet-changed', (e) => {
        if (e.detail && e.detail.context) {
            activeWalletContext = e.detail.context;
            updateWalletContextView();
        }
    });

    // 6. Identity Update
    function updateTelemetryIdentity(msg) {
        if (chartAssetTitle && msg.symbol) {
            const prefix = activeWalletContext === 'spot' ? 'SPOT' : 'PERP';
            chartAssetTitle.textContent = `${msg.symbol} (${prefix} DOLLAR BARS)`;
        }
    }

    // 7. Time Filter Helper
    function isWithinTimeFilter(timestamp) {
        if (!timestamp || activeTimeFilter === 'all' || activeTimeFilter === 'session') return true;
        const now = Date.now();
        const diffMs = now - timestamp;
        if (activeTimeFilter === '1h') return diffMs <= 3600 * 1000;
        if (activeTimeFilter === '24h') return diffMs <= 86400 * 1000;
        return true;
    }

    // 8. Operational Tables Rendering
    function renderPositionsTable(pos, mark, upnl) {
        if (!positionsBody) return;
        positionsBody.innerHTML = '';

        if (!pos) {
            const tr = document.createElement('tr');
            tr.innerHTML = '<td colspan="9" style="text-align: center; color: var(--text-muted); padding: 16px;">Sin posición abierta (FLAT)</td>';
            positionsBody.appendChild(tr);
            return;
        }

        const tr = document.createElement('tr');
        const entry = parseFloat(pos.entry_price);
        const qty = parseFloat(pos.quantity);
        const sl = parseFloat(pos.stop_loss);
        const tp = parseFloat(pos.take_profit);
        const markVal = mark ? parseFloat(mark) : entry;
        const upnlVal = upnl !== undefined ? parseFloat(upnl) : ((markVal - entry) * qty);
        const marginVal = activeWalletContext === 'spot' ? 'N/A' : `$${(entry * qty).toFixed(2)}`;
        const upnlTone = upnlVal > 0 ? 'positive' : upnlVal < 0 ? 'negative' : 'neutral';

        tr.innerHTML = `
            <td><strong>BTCUSDT</strong></td>
            <td><span class="position-badge ${(pos.side || 'Long').toLowerCase()}">${(pos.side || 'Long').toUpperCase()}</span></td>
            <td>${qty.toFixed(4)} BTC</td>
            <td>$${entry.toFixed(2)}</td>
            <td>$${markVal.toFixed(2)}</td>
            <td style="color: var(--negative-red);">$${sl.toFixed(2)}</td>
            <td style="color: var(--positive-green);">$${tp.toFixed(2)}</td>
            <td class="${upnlTone}">${upnlVal >= 0 ? '+' : ''}$${upnlVal.toFixed(2)}</td>
            <td>${marginVal}</td>
        `;
        positionsBody.appendChild(tr);
    }

    function renderOrdersTable() {
        if (!ordersBody) return;
        ordersBody.innerHTML = '';

        if (sessionOrders.length === 0) {
            const tr = document.createElement('tr');
            tr.innerHTML = '<td colspan="8" style="text-align: center; color: var(--text-muted); padding: 16px;">Sin órdenes pendientes en el libro</td>';
            ordersBody.appendChild(tr);
            return;
        }

        sessionOrders.forEach(ord => {
            const tr = document.createElement('tr');
            const timeStr = new Date(ord.timestamp).toLocaleTimeString();
            tr.innerHTML = `
                <td><code>${ord.id}</code></td>
                <td>${ord.symbol}</td>
                <td>${ord.type}</td>
                <td><span class="${ord.side === 'BUY' ? 'positive' : 'negative'}">${ord.side}</span></td>
                <td>${ord.quantity.toFixed(4)}</td>
                <td>$${ord.price.toFixed(2)}</td>
                <td><span class="badge badge-paper">${ord.status}</span></td>
                <td>${timeStr}</td>
            `;
            ordersBody.appendChild(tr);
        });
    }

    function renderFillsTable() {
        if (!fillsBody) return;
        fillsBody.innerHTML = '';
        const filtered = sessionFills.filter(f => isWithinTimeFilter(f.timestamp));

        if (fillsCount) fillsCount.textContent = sessionFills.length;

        if (filtered.length === 0) {
            const tr = document.createElement('tr');
            tr.innerHTML = '<td colspan="8" style="text-align: center; color: var(--text-muted); padding: 16px;">Sin ejecuciones en el intervalo seleccionado</td>';
            fillsBody.appendChild(tr);
            return;
        }

        filtered.slice(0, 50).forEach(fill => {
            const tr = document.createElement('tr');
            const timeStr = new Date(fill.timestamp).toLocaleTimeString();
            tr.innerHTML = `
                <td><code>${fill.id}</code></td>
                <td>${timeStr}</td>
                <td>${fill.symbol}</td>
                <td><span class="${fill.side === 'BUY' ? 'positive' : 'negative'}">${fill.side}</span></td>
                <td>$${fill.price.toFixed(2)}</td>
                <td>${fill.quantity.toFixed(4)}</td>
                <td>$${fill.fee.toFixed(2)}</td>
                <td><span class="badge badge-demo">${fill.execution_type}</span></td>
            `;
            fillsBody.appendChild(tr);
        });
    }

    function renderTradesTable() {
        if (!tradesBody) return;
        tradesBody.innerHTML = '';
        const filtered = sessionClosedTrades.filter(t => isWithinTimeFilter(t.timestamp));

        if (tradesCount) tradesCount.textContent = sessionClosedTrades.length;
        if (kpiTradesSummary) kpiTradesSummary.textContent = `${sessionClosedTrades.length} trades`;

        if (filtered.length === 0) {
            const tr = document.createElement('tr');
            tr.innerHTML = '<td colspan="9" style="text-align: center; color: var(--text-muted); padding: 16px;">Sin operaciones cerradas en el intervalo seleccionado</td>';
            tradesBody.appendChild(tr);
            return;
        }

        filtered.slice(0, 50).forEach(trade => {
            const tr = document.createElement('tr');
            const timeStr = new Date(trade.timestamp).toLocaleTimeString();
            const pnlTone = trade.net_pnl > 0 ? 'positive' : trade.net_pnl < 0 ? 'negative' : 'neutral';
            tr.innerHTML = `
                <td><code>${trade.id}</code></td>
                <td>${timeStr}</td>
                <td>${trade.symbol}</td>
                <td>${trade.side}</td>
                <td>$${trade.entry_price.toFixed(2)}</td>
                <td>$${trade.exit_price.toFixed(2)}</td>
                <td>${trade.quantity.toFixed(4)}</td>
                <td>${trade.exit_reason}</td>
                <td class="${pnlTone}"><strong>${trade.net_pnl >= 0 ? '+' : ''}$${trade.net_pnl.toFixed(2)}</strong></td>
            `;
            tradesBody.appendChild(tr);
        });
    }

    function appendLog(eventType, symbol, price, details, timestamp = null, strategyId = '') {
        const tr = document.createElement('tr');
        const eventTime = timestamp === null ? '---' : new Date(timestamp).toLocaleTimeString();

        sessionEvents.unshift({ eventType, symbol, price, details, timestamp, strategyId });
        if (eventsCount) eventsCount.textContent = sessionEvents.length;

        let badgeColor = 'neutral';
        if (eventType.includes('OPEN') || eventType.includes('BAR')) badgeColor = 'positive';
        if (eventType.includes('CLOSE')) badgeColor = (details || '').includes('-') ? 'negative' : 'positive';

        const values = [eventTime, eventType, symbol, price || '---',
            `${strategyId ? `[${strategyId}] ` : ''}${details || ''}`];

        values.forEach((value, index) => {
            const cell = document.createElement('td');
            cell.textContent = value;
            if (index === 0 || index === 4) cell.style.color = 'var(--text-muted)';
            if (index === 1) {
                cell.style.fontWeight = '700';
                cell.style.color = 'var(--accent-blue)';
            }
            if (index === 3) cell.className = badgeColor;
            tr.appendChild(cell);
        });

        if (eventsBody) {
            eventsBody.insertBefore(tr, eventsBody.firstChild);
            if (eventsBody.children.length > 80) {
                eventsBody.removeChild(eventsBody.lastChild);
            }
        }
    }

    // 9. Interactive Tab & Time Filter Navigation
    function initOperationalNavigation() {
        // Operational Tabs
        document.querySelectorAll('.op-tab-btn').forEach(btn => {
            btn.addEventListener('click', () => {
                document.querySelectorAll('.op-tab-btn').forEach(b => b.classList.remove('active'));
                btn.classList.add('active');

                const tab = btn.getAttribute('data-op-tab');
                document.querySelectorAll('.op-pane').forEach(p => p.style.display = 'none');

                const targetPane = document.getElementById(`op-pane-${tab}`);
                if (targetPane) {
                    targetPane.style.display = 'block';
                }
            });
        });

        // Time Filters
        document.querySelectorAll('.time-filter-btn').forEach(btn => {
            btn.addEventListener('click', () => {
                document.querySelectorAll('.time-filter-btn').forEach(b => b.classList.remove('active'));
                btn.classList.add('active');
                activeTimeFilter = btn.getAttribute('data-time-filter') || 'all';
                renderFillsTable();
                renderTradesTable();
            });
        });
    }

    // 10. Freshness and Heartbeat Monitor (Timer every second)
    function updateFreshness() {
        const now = Date.now();
        if (lastEventTimestamp && kpiFreshness) {
            const diffSecs = Math.floor((now - lastEventTimestamp) / 1000);
            kpiFreshness.textContent = `${diffSecs}s ago`;
            kpiFreshness.className = `metric-value ${diffSecs < 10 ? 'positive' : diffSecs < 30 ? 'neutral' : 'negative'}`;
        }
    }

    // 11. Health Polling
    function pollHealth() {
        fetch('/api/health')
            .then(res => res.json())
            .then(data => {
                if (data.execution_mode) {
                    executionMode = data.execution_mode;
                    applyExecutionMode(executionMode);
                }
                if (kpiClientsFresh) {
                    kpiClientsFresh.textContent = `WS: ${data.connected_ws_clients} activos`;
                }
            })
            .catch(() => {});
    }

    // 12. Research Reports Index & Viewer
    function loadReportsIndex() {
        fetch('/api/reports')
            .then(res => res.json())
            .then(reports => {
                const btSelect = document.getElementById('backtest-select');
                const hpoSelect = document.getElementById('hpo-select');
                if (!btSelect || !hpoSelect) return;

                btSelect.innerHTML = '<option value="">Select an audit report from storage/reports...</option>';
                hpoSelect.innerHTML = '<option value="">Select HPO or Monte Carlo Report...</option>';

                reports.forEach(r => {
                    const opt = document.createElement('option');
                    opt.value = r.filename;
                    const dateStr = new Date(r.modified_timestamp * 1000).toLocaleString();
                    opt.textContent = `${r.filename} (${r.report_type.toUpperCase()}) - ${dateStr}`;

                    if (r.report_type === 'hpo') {
                        hpoSelect.appendChild(opt);
                    } else {
                        btSelect.appendChild(opt);
                    }
                });
            })
            .catch(err => console.error('Error fetching reports:', err));
    }

    // Load selected Backtest report
    const btnLoadReport = document.getElementById('btn-load-report');
    if (btnLoadReport) {
        btnLoadReport.addEventListener('click', () => {
            const filename = document.getElementById('backtest-select').value;
            if (!filename) return;

            fetch(`/api/reports/${filename}`)
                .then(res => res.json())
                .then(report => {
                    const metrics = report.metrics || {};
                    const netProfit = parseFloat(metrics.net_profit || 0);
                    const winRate = parseFloat(metrics.win_rate || 0);

                    const pnlEl = document.getElementById('bt-pnl');
                    if (pnlEl) {
                        pnlEl.textContent = `${netProfit >= 0 ? '+' : ''}$${netProfit.toFixed(2)}`;
                        pnlEl.className = `kpi-value ${netProfit >= 0 ? 'positive' : 'negative'}`;
                    }

                    const winRateEl = document.getElementById('bt-winrate');
                    if (winRateEl) winRateEl.textContent = `${(winRate * 100).toFixed(1)}%`;
                    const sortinoEl = document.getElementById('bt-sortino');
                    if (sortinoEl) sortinoEl.textContent = parseFloat(metrics.sortino_ratio || 0).toFixed(2);
                    const tradesEl = document.getElementById('bt-trades');
                    if (tradesEl) tradesEl.textContent = metrics.total_trades || '0';
                    const ddEl = document.getElementById('bt-drawdown');
                    if (ddEl) ddEl.textContent = `${(parseFloat(metrics.max_drawdown_pct || 0) * 100).toFixed(2)}%`;

                    renderBacktestChart(report);
                })
                .catch(err => alert('Failed loading report: ' + err));
        });
    }

    function renderBacktestChart(report) {
        const container = document.getElementById('chart-backtest');
        if (!container || typeof LightweightCharts === 'undefined') return;
        container.innerHTML = '';

        backtestChart = LightweightCharts.createChart(container, {
            layout: { background: { color: '#111827' }, textColor: '#94a3b8' },
            grid: { vertLines: { color: '#1e293b' }, horzLines: { color: '#1e293b' } },
            timeScale: { timeVisible: true },
        });

        backtestSeries = backtestChart.addLineSeries({
            color: '#10b981',
            lineWidth: 2,
        });

        const initial = parseFloat(report.initial_capital || 10000);
        const finalEq = parseFloat(report.final_equity || initial);
        const points = [
            { time: Math.floor(Date.now() / 1000) - 3600, value: initial },
            { time: Math.floor(Date.now() / 1000), value: finalEq },
        ];
        backtestSeries.setData(points);
        backtestChart.timeScale().fitContent();
    }

    // Load HPO / Monte Carlo
    const btnLoadHpo = document.getElementById('btn-load-hpo');
    if (btnLoadHpo) {
        btnLoadHpo.addEventListener('click', () => {
            const filename = document.getElementById('hpo-select').value;
            if (!filename) return;

            fetch(`/api/reports/${filename}`)
                .then(res => res.json())
                .then(report => {
                    renderMonteCarloChart(report);
                })
                .catch(err => alert('Failed loading HPO/MC report: ' + err));
        });
    }

    function renderMonteCarloChart(report) {
        const container = document.getElementById('chart-montecarlo');
        if (!container || typeof LightweightCharts === 'undefined') return;
        container.innerHTML = '';

        hpoChart = LightweightCharts.createChart(container, {
            layout: { background: { color: '#111827' }, textColor: '#94a3b8' },
            grid: { vertLines: { color: '#1e293b' }, horzLines: { color: '#1e293b' } },
        });

        const curves = report.fan_chart_curves || report.fan_chart_trajectories;
        if (curves && Array.isArray(curves)) {
            curves.slice(0, 30).forEach((curve, idx) => {
                const s = hpoChart.addLineSeries({
                    color: idx === 0 ? '#10b981' : 'rgba(56, 189, 248, 0.25)',
                    lineWidth: idx === 0 ? 2 : 1,
                });
                const data = curve.map((val, i) => ({ time: i + 1, value: parseFloat(val) }));
                s.setData(data);
            });
            hpoChart.timeScale().fitContent();
        }
    }

    // 13. System Startup Bootstrap
    function init() {
        initLiveChart();
        initOperationalNavigation();
        updateWalletContextView();

        fetch('/api/state')
            .then(res => {
                if (!res.ok) throw new Error('Snapshot request failed');
                return res.json();
            })
            .then(updateInitialSnapshot)
            .catch(err => console.error('Failed loading initial state:', err))
            .finally(connectWebSocket);

        loadReportsIndex();
        pollHealth();
        setInterval(pollHealth, 3000);
        setInterval(updateFreshness, 1000);
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }
})();
