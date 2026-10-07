// ATSNT Quantitative Web Dashboard Controller
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
    let eventLogCount = 0;
    let chartMarkers = [];
    let telemetryStale = false;

    // DOM Elements
    const wsStatus = document.getElementById('ws-status');
    const wsStatusText = document.getElementById('ws-status-text');
    const livePriceEl = document.getElementById('live-price');
    const kpiEquity = document.getElementById('kpi-equity');
    const kpiCash = document.getElementById('kpi-cash');
    const kpiUpnl = document.getElementById('kpi-upnl');
    const kpiUpnlPct = document.getElementById('kpi-upnl-pct');
    const kpiPosition = document.getElementById('kpi-position');
    const kpiPositionDetails = document.getElementById('kpi-position-details');
    const kpiDrawdown = document.getElementById('kpi-drawdown');
    const kpiUptime = document.getElementById('kpi-uptime');
    const kpiClients = document.getElementById('kpi-clients');

    const posBadge = document.getElementById('pos-badge');
    const posEntry = document.getElementById('pos-entry');
    const posMark = document.getElementById('pos-mark');
    const posQty = document.getElementById('pos-qty');
    const posSl = document.getElementById('pos-sl');
    const posTp = document.getElementById('pos-tp');

    const eventsBody = document.getElementById('events-body');
    const eventsCount = document.getElementById('events-count');

    // Tab Navigation
    document.querySelectorAll('.nav-tab').forEach(btn => {
        btn.addEventListener('click', () => {
            document.querySelectorAll('.nav-tab').forEach(b => b.classList.remove('active'));
            document.querySelectorAll('.tab-pane').forEach(p => p.classList.remove('active'));

            btn.classList.add('active');
            const targetTab = btn.getAttribute('data-tab');
            const pane = document.getElementById(`tab-${targetTab}`);
            if (pane) pane.classList.add('active');

            // Resize charts on tab switch
            if (targetTab === 'live' && liveChart) liveChart.timeScale().fitContent();
            if (targetTab === 'backtest' && backtestChart) backtestChart.timeScale().fitContent();
            if (targetTab === 'hpo' && hpoChart) hpoChart.timeScale().fitContent();
        });
    });

    // Initialize TradingView Candlestick Chart
    function initLiveChart() {
        const container = document.getElementById('chart-live');
        if (!container || typeof LightweightCharts === 'undefined') return;

        liveChart = LightweightCharts.createChart(container, {
            layout: {
                background: { color: '#121824' },
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

    // Connect WebSocket
    function connectWebSocket() {
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const wsUrl = `${protocol}//${window.location.host}/ws/telemetry`;

        wsStatus.className = 'status-badge';
        wsStatusText.textContent = 'CONNECTING...';

        try {
            socket = new WebSocket(wsUrl);
        } catch (e) {
            scheduleReconnect();
            return;
        }

        socket.onopen = function () {
            showTelemetryStatus();
            reconnectDelay = 1000;
            appendLog('SYSTEM', '---', 'Connected to real-time telemetry stream');
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
            wsStatus.className = 'status-badge';
            wsStatusText.textContent = 'DISCONNECTED';
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

    // Handle inbound telemetry messages
    function handleTelemetryEvent(msg) {
        const type = msg.event_type;
        const p = msg.payload;
        const symbol = msg.symbol;
        const log = (...args) => appendLog(...args, msg.timestamp, msg.strategy_id);
        updateTelemetryIdentity(msg);

        if (type === 'InitialSnapshot') {
            updateInitialSnapshot(p);
            return;
        }

        if (type === 'BarFormed') {
            const bar = p.BarFormed || p;
            const candleTime = Math.floor(bar.end_time / 1000);
            const closeVal = parseFloat(bar.close);

            if (candleSeries) {
                candleSeries.update({
                    time: candleTime,
                    open: parseFloat(bar.open),
                    high: parseFloat(bar.high),
                    low: parseFloat(bar.low),
                    close: closeVal,
                });
            }

            livePriceEl.textContent = `$${closeVal.toLocaleString('en-US', { minimumFractionDigits: 2 })}`;
            log('BAR_FORMED', symbol, `$${closeVal.toFixed(2)}`, `Vol: ${parseFloat(bar.volume).toFixed(4)} base units ($${parseFloat(bar.dollar_volume).toFixed(0)})`);
        } else if (type === 'MarkToMarket') {
            const m = p.MarkToMarket || p;
            const price = parseFloat(m.current_price);
            const upnl = parseFloat(m.unrealized_pnl);
            const totalEq = parseFloat(m.total_equity);
            const dd = parseFloat(m.drawdown_pct);

            livePriceEl.textContent = `$${price.toLocaleString('en-US', { minimumFractionDigits: 2 })}`;
            posMark.textContent = `$${price.toFixed(2)}`;
            kpiEquity.textContent = `$${totalEq.toLocaleString('en-US', { minimumFractionDigits: 2 })}`;
            kpiDrawdown.textContent = `${(dd * 100).toFixed(2)}%`;

            kpiUpnl.textContent = `${upnl >= 0 ? '+' : ''}$${upnl.toFixed(2)}`;
            kpiUpnl.className = `kpi-value ${upnl > 0 ? 'positive' : upnl < 0 ? 'negative' : 'neutral'}`;

            const upnlPct = (upnl / totalEq) * 100;
            kpiUpnlPct.textContent = `${upnlPct >= 0 ? '+' : ''}${upnlPct.toFixed(2)}%`;
            kpiUpnlPct.className = `kpi-sub ${upnl > 0 ? 'positive' : upnl < 0 ? 'negative' : 'neutral'}`;
        } else if (type === 'PositionOpened') {
            const pos = p.PositionOpened || p;
            const side = pos.side;
            const entry = parseFloat(pos.entry_price);
            const qty = parseFloat(pos.quantity);
            const sl = parseFloat(pos.stop_loss);
            const tp = parseFloat(pos.take_profit);

            posBadge.className = `position-badge ${side.toLowerCase()}`;
            posBadge.textContent = side.toUpperCase();
            kpiPosition.textContent = `${side.toUpperCase()} ${qty} base units`;
            kpiPositionDetails.textContent = `@ $${entry.toFixed(2)}`;

            posEntry.textContent = `$${entry.toFixed(2)}`;
            posQty.textContent = `${qty.toFixed(4)} base units`;
            posSl.textContent = `$${sl.toFixed(2)}`;
            posTp.textContent = `$${tp.toFixed(2)}`;

            // Marker on chart
            if (candleSeries) {
                chartMarkers.push({
                    time: Math.floor(msg.timestamp / 1000),
                    position: side.toLowerCase() === 'long' ? 'belowBar' : 'aboveBar',
                    color: side.toLowerCase() === 'long' ? '#10b981' : '#f43f5e',
                    shape: side.toLowerCase() === 'long' ? 'arrowUp' : 'arrowDown',
                    text: `ENTRY ${side.toUpperCase()} @ ${entry.toFixed(0)}`,
                });
                candleSeries.setMarkers(chartMarkers);
            }

            log('POSITION_OPEN', symbol, `$${entry.toFixed(2)}`, `${side.toUpperCase()} ${qty} base units (SL: ${sl.toFixed(0)}, TP: ${tp.toFixed(0)})`);
        } else if (type === 'PositionClosed') {
            const exit = p.PositionClosed || p;
            const pnl = parseFloat(exit.net_pnl);
            const exitPrice = parseFloat(exit.exit_price);
            const totalEq = parseFloat(exit.total_equity);

            posBadge.className = 'position-badge flat';
            posBadge.textContent = 'FLAT';
            kpiPosition.textContent = 'FLAT';
            kpiPositionDetails.textContent = '0.00 base units ($0.00)';

            posEntry.textContent = '---';
            posMark.textContent = '---';
            posQty.textContent = '---';
            posSl.textContent = '---';
            posTp.textContent = '---';

            kpiEquity.textContent = `$${totalEq.toLocaleString('en-US', { minimumFractionDigits: 2 })}`;
            kpiCash.textContent = `Cash: $${totalEq.toLocaleString('en-US', { minimumFractionDigits: 2 })}`;
            kpiUpnl.textContent = '$0.00';
            kpiUpnl.className = 'kpi-value neutral';

            // Marker on chart
            if (candleSeries) {
                chartMarkers.push({
                    time: Math.floor(msg.timestamp / 1000),
                    position: 'aboveBar',
                    color: pnl >= 0 ? '#10b981' : '#f43f5e',
                    shape: 'circle',
                    text: `EXIT (${exit.exit_reason}) ${pnl >= 0 ? '+' : ''}$${pnl.toFixed(2)}`,
                });
                candleSeries.setMarkers(chartMarkers);
            }

            log('POSITION_CLOSED', symbol, `$${exitPrice.toFixed(2)}`, `Reason: ${exit.exit_reason}, Net PnL: ${pnl >= 0 ? '+' : ''}$${pnl.toFixed(2)}`);
        } else if (type === 'SignalGenerated') {
            const s = p.SignalGenerated || p;
            log('SIGNAL', symbol, `$${parseFloat(s.price).toFixed(2)}`, `Signal: ${s.side}`);
        }
    }

    function updateTelemetryIdentity(msg) {
        for (const [id, value] of [['asset-selector', msg.symbol], ['strategy-selector', msg.strategy_id]]) {
            const selector = document.getElementById(id);
            if (!selector) continue;
            if (!value) {
                selector.selectedIndex = -1;
                continue;
            }
            let option = Array.from(selector.options).find(item => item.value === value);
            if (!option) {
                option = document.createElement('option');
                option.value = value;
                option.textContent = value;
                selector.appendChild(option);
            }
            option.disabled = false;
            selector.value = value;
        }
        const title = document.querySelector('#chart-live')?.parentElement?.querySelector('.panel-title > span');
        if (title) title.textContent = msg.symbol ? `${msg.symbol} (DOLLAR BARS)` : 'Awaiting configured telemetry';
    }

    function showTelemetryStatus() {
        wsStatus.className = telemetryStale ? 'status-badge' : 'status-badge live';
        wsStatusText.textContent = telemetryStale ? 'STALE / UNCERTAIN' : 'LIVE WS';
    }

    function updateInitialSnapshot(st) {
        if (!st) return;
        // A last-known projection is not an authoritative resync, even on reconnect.
        telemetryStale = telemetryStale || st.stale === true;
        showTelemetryStatus();
        updateTelemetryIdentity({ symbol: st.active_symbol, strategy_id: st.active_strategy });
        const equity = parseFloat(st.portfolio_value);
        const upnl = parseFloat(st.unrealized_pnl);
        const tone = upnl > 0 ? 'positive' : upnl < 0 ? 'negative' : 'neutral';
        kpiEquity.textContent = `$${equity.toLocaleString('en-US', { minimumFractionDigits: 2 })}`;
        kpiCash.textContent = `Cash: $${parseFloat(st.cash_balance).toLocaleString('en-US', { minimumFractionDigits: 2 })}`;
        kpiUpnl.textContent = `${upnl >= 0 ? '+' : ''}$${upnl.toFixed(2)}`;
        kpiUpnl.className = `kpi-value ${tone}`;
        const pct = equity === 0 ? 0 : upnl / equity * 100;
        kpiUpnlPct.textContent = `${pct >= 0 ? '+' : ''}${pct.toFixed(2)}%`;
        kpiUpnlPct.className = `kpi-sub ${tone}`;
        kpiDrawdown.textContent = `${(parseFloat(st.drawdown_pct) * 100).toFixed(2)}%`;
        const mark = st.last_price === null ? null : parseFloat(st.last_price);
        livePriceEl.textContent = mark === null ? '---' : `$${mark.toLocaleString('en-US', { minimumFractionDigits: 2 })}`;
        const pos = st.active_position;
        if (pos) {
            const side = pos.side;
            const entry = parseFloat(pos.entry_price);
            const qty = parseFloat(pos.quantity);
            posBadge.className = `position-badge ${side.toLowerCase()}`;
            posBadge.textContent = side.toUpperCase();
            kpiPosition.textContent = `${side.toUpperCase()} ${qty} base units`;
            kpiPositionDetails.textContent = `@ $${entry.toFixed(2)}`;
            posEntry.textContent = `$${entry.toFixed(2)}`;
            posQty.textContent = `${qty.toFixed(4)} base units`;
            posSl.textContent = `$${parseFloat(pos.stop_loss).toFixed(2)}`;
            posTp.textContent = `$${parseFloat(pos.take_profit).toFixed(2)}`;
            posMark.textContent = mark === null ? '---' : `$${mark.toFixed(2)}`;
        } else {
            posBadge.className = 'position-badge flat';
            posBadge.textContent = 'FLAT';
            kpiPosition.textContent = 'FLAT';
            kpiPositionDetails.textContent = '0.00 base units ($0.00)';
            for (const element of [posEntry, posMark, posQty, posSl, posTp]) element.textContent = '---';
        }
    }

    function appendLog(eventType, symbol, price, details, timestamp = null, strategyId = '') {
        eventLogCount++;
        eventsCount.textContent = `${eventLogCount} events`;

        const tr = document.createElement('tr');
        const eventTime = timestamp === null ? '---' : new Date(timestamp).toLocaleTimeString();

        let badgeColor = 'neutral';
        if (eventType.includes('OPEN') || eventType.includes('BAR')) badgeColor = 'positive';
        if (eventType.includes('CLOSE')) badgeColor = (details || '').includes('-') ? 'negative' : 'positive';

        // Configured identities and event details are text, never executable markup.
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

        eventsBody.insertBefore(tr, eventsBody.firstChild);
        if (eventsBody.children.length > 80) {
            eventsBody.removeChild(eventsBody.lastChild);
        }
    }

    // Health poll
    function pollHealth() {
        fetch('/api/health')
            .then(res => res.json())
            .then(data => {
                kpiUptime.textContent = `${data.uptime_secs}s`;
                kpiClients.textContent = `WS Clients: ${data.connected_ws_clients}`;
            })
            .catch(() => {});
    }

    // Reports Index & Viewer
    function loadReportsIndex() {
        fetch('/api/reports')
            .then(res => res.json())
            .then(reports => {
                const btSelect = document.getElementById('backtest-select');
                const hpoSelect = document.getElementById('hpo-select');

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
    document.getElementById('btn-load-report').addEventListener('click', () => {
        const filename = document.getElementById('backtest-select').value;
        if (!filename) return;

        fetch(`/api/reports/${filename}`)
            .then(res => res.json())
            .then(report => {
                const metrics = report.metrics || {};
                const netProfit = parseFloat(metrics.net_profit || 0);
                const winRate = parseFloat(metrics.win_rate || 0);

                const pnlEl = document.getElementById('bt-pnl');
                pnlEl.textContent = `${netProfit >= 0 ? '+' : ''}$${netProfit.toFixed(2)}`;
                pnlEl.className = `kpi-value ${netProfit >= 0 ? 'positive' : 'negative'}`;

                document.getElementById('bt-winrate').textContent = `${(winRate * 100).toFixed(1)}%`;
                document.getElementById('bt-sortino').textContent = parseFloat(metrics.sortino_ratio || 0).toFixed(2);
                document.getElementById('bt-trades').textContent = metrics.total_trades || '0';
                document.getElementById('bt-drawdown').textContent = `${(parseFloat(metrics.max_drawdown_pct || 0) * 100).toFixed(2)}%`;

                renderBacktestChart(report);
            })
            .catch(err => alert('Failed loading report: ' + err));
    });

    function renderBacktestChart(report) {
        const container = document.getElementById('chart-backtest');
        container.innerHTML = '';

        backtestChart = LightweightCharts.createChart(container, {
            layout: { background: { color: '#121824' }, textColor: '#94a3b8' },
            grid: { vertLines: { color: '#1e293b' }, horzLines: { color: '#1e293b' } },
            timeScale: { timeVisible: true },
        });

        backtestSeries = backtestChart.addLineSeries({
            color: '#10b981',
            lineWidth: 2,
        });

        // Sample synthetic curve from initial to final equity
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
    document.getElementById('btn-load-hpo').addEventListener('click', () => {
        const filename = document.getElementById('hpo-select').value;
        if (!filename) return;

        fetch(`/api/reports/${filename}`)
            .then(res => res.json())
            .then(report => {
                renderMonteCarloChart(report);
            })
            .catch(err => alert('Failed loading HPO/MC report: ' + err));
    });

    function renderMonteCarloChart(report) {
        const container = document.getElementById('chart-montecarlo');
        container.innerHTML = '';

        hpoChart = LightweightCharts.createChart(container, {
            layout: { background: { color: '#121824' }, textColor: '#94a3b8' },
            grid: { vertLines: { color: '#1e293b' }, horzLines: { color: '#1e293b' } },
        });

        // If fan chart lines are present in the report
        if (report.fan_chart_curves && Array.isArray(report.fan_chart_curves)) {
            report.fan_chart_curves.slice(0, 30).forEach((curve, idx) => {
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

    // Startup
    initLiveChart();
    fetch('/api/state')
        .then(res => {
            if (!res.ok) throw new Error('Snapshot request failed');
            return res.json();
        })
        .then(updateInitialSnapshot)
        .catch(err => console.error('Failed loading initial state:', err))
        .finally(connectWebSocket);
    loadReportsIndex();
    setInterval(pollHealth, 3000);

})();
