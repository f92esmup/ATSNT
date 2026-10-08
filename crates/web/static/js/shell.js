// ATSNT Read-Only Web Workspace — Shell & Layout Controller (W1)
(function (global) {
    'use strict';

    const STORAGE_KEY_RAIL = 'atsnt_rail_expanded';

    // DOM Cache
    const globalRail = document.getElementById('global-rail');
    const btnToggleRail = document.getElementById('btn-toggle-rail');
    const drawerBackdrop = document.getElementById('drawer-backdrop');
    const mobileDrawer = document.getElementById('mobile-drawer');
    const btnOpenDrawer = document.getElementById('btn-open-drawer');
    const btnCloseDrawer = document.getElementById('btn-close-drawer');
    const clockUtc = document.getElementById('clock-utc');
    const clockLocal = document.getElementById('clock-local');
    const clockTz = document.getElementById('clock-tz');
    const breadcrumbSection = document.getElementById('breadcrumb-section');
    const walletSelect = document.getElementById('wallet-select');
    const btnLangToggle = document.getElementById('btn-lang-toggle');

    let lastFocusedElement = null;

    // 1. Dual Clock Engine
    function updateClock() {
        const now = new Date();

        // UTC Formatting
        const utcHours = String(now.getUTCHours()).padStart(2, '0');
        const utcMinutes = String(now.getUTCMinutes()).padStart(2, '0');
        const utcSeconds = String(now.getUTCSeconds()).padStart(2, '0');
        if (clockUtc) {
            clockUtc.textContent = `${utcHours}:${utcMinutes}:${utcSeconds}`;
        }

        // Local Formatting
        const localHours = String(now.getHours()).padStart(2, '0');
        const localMinutes = String(now.getMinutes()).padStart(2, '0');
        const localSeconds = String(now.getSeconds()).padStart(2, '0');
        if (clockLocal) {
            clockLocal.textContent = `${localHours}:${localMinutes}:${localSeconds}`;
        }

        // Timezone abbreviation
        if (clockTz) {
            const tzName = Intl.DateTimeFormat().resolvedOptions().timeZone || 'Local';
            clockTz.textContent = tzName.split('/').pop().replace('_', ' ');
        }
    }

    // 2. Global Rail Collapse / Expand
    function initRail() {
        const isExpanded = localStorage.getItem(STORAGE_KEY_RAIL) === 'true';
        if (globalRail && isExpanded) {
            globalRail.classList.add('expanded');
            if (btnToggleRail) btnToggleRail.setAttribute('aria-expanded', 'true');
        }

        if (btnToggleRail && globalRail) {
            btnToggleRail.addEventListener('click', () => {
                const expanded = globalRail.classList.toggle('expanded');
                localStorage.setItem(STORAGE_KEY_RAIL, expanded);
                btnToggleRail.setAttribute('aria-expanded', String(expanded));
            });
        }
    }

    // 3. Accessible Mobile Drawer (<768px)
    function openDrawer() {
        lastFocusedElement = document.activeElement;
        if (drawerBackdrop) {
            drawerBackdrop.classList.add('open');
            drawerBackdrop.setAttribute('aria-hidden', 'false');
        }
        if (btnCloseDrawer) {
            btnCloseDrawer.focus();
        }
        document.body.style.overflow = 'hidden';
    }

    function closeDrawer() {
        if (drawerBackdrop) {
            drawerBackdrop.classList.remove('open');
            drawerBackdrop.setAttribute('aria-hidden', 'true');
        }
        document.body.style.overflow = '';
        if (lastFocusedElement) {
            lastFocusedElement.focus();
        }
    }

    function initDrawer() {
        if (btnOpenDrawer) {
            btnOpenDrawer.addEventListener('click', openDrawer);
        }
        if (btnCloseDrawer) {
            btnCloseDrawer.addEventListener('click', closeDrawer);
        }
        if (drawerBackdrop) {
            drawerBackdrop.addEventListener('click', (e) => {
                if (e.target === drawerBackdrop) {
                    closeDrawer();
                }
            });
        }

        // Keyboard navigation: Escape key closes drawer
        window.addEventListener('keydown', (e) => {
            if (e.key === 'Escape' && drawerBackdrop && drawerBackdrop.classList.contains('open')) {
                closeDrawer();
            }
        });
    }

    // 4. Modular View Routing (Overview, Operations, Laboratory, Strategies, Data, System)
    function switchModule(moduleId) {
        const targetViewId = `view-${moduleId}`;
        const targetView = document.getElementById(targetViewId);

        if (!targetView) return;

        // Update nav buttons in both desktop rail and mobile drawer
        document.querySelectorAll('.module-btn').forEach(btn => {
            const btnMod = btn.getAttribute('data-module');
            if (btnMod === moduleId) {
                btn.classList.add('active');
                btn.setAttribute('aria-current', 'page');
            } else {
                btn.classList.remove('active');
                btn.removeAttribute('aria-current');
            }
        });

        // Toggle view visibility
        document.querySelectorAll('.view-section').forEach(view => {
            view.classList.remove('active');
        });
        targetView.classList.add('active');

        // Update breadcrumb
        if (breadcrumbSection && global.I18n) {
            const i18nKey = `module.${moduleId}`;
            breadcrumbSection.textContent = global.I18n.t(i18nKey);
            breadcrumbSection.setAttribute('data-i18n', i18nKey);
        }

        // Close drawer if open on mobile
        closeDrawer();

        // Update URL hash safely without jump
        if (window.location.hash !== `#${moduleId}`) {
            history.replaceState(null, '', `#${moduleId}`);
        }

        // Resize charts if switching to operations
        if (moduleId === 'operations' && window.dispatchEvent) {
            window.dispatchEvent(new Event('resize'));
        }
    }

    function initRouting() {
        document.querySelectorAll('.module-btn').forEach(btn => {
            btn.addEventListener('click', (e) => {
                e.preventDefault();
                const moduleId = btn.getAttribute('data-module');
                if (moduleId) {
                    switchModule(moduleId);
                }
            });
        });

        // Hash-based initial view
        const initialHash = window.location.hash.replace('#', '');
        const validModules = ['overview', 'operations', 'laboratory', 'strategies', 'data', 'system'];
        if (validModules.includes(initialHash)) {
            switchModule(initialHash);
        } else {
            switchModule('operations'); // Default to Operations view
        }

        window.addEventListener('hashchange', () => {
            const hash = window.location.hash.replace('#', '');
            if (validModules.includes(hash)) {
                switchModule(hash);
            }
        });
    }

    // 5. Context & Language Controls
    function initControls() {
        if (walletSelect) {
            walletSelect.addEventListener('change', () => {
                const selectedContext = walletSelect.value;
                window.dispatchEvent(new CustomEvent('atsnt:wallet-changed', {
                    detail: { context: selectedContext }
                }));
            });
        }

        if (btnLangToggle && global.I18n) {
            const updateLangBtn = () => {
                const current = global.I18n.getLocale();
                btnLangToggle.textContent = current.toUpperCase();
                btnLangToggle.setAttribute('aria-label', `Language: ${current.toUpperCase()}`);
            };
            updateLangBtn();

            btnLangToggle.addEventListener('click', () => {
                const current = global.I18n.getLocale();
                const next = current === 'es' ? 'en' : 'es';
                global.I18n.setLocale(next);
                updateLangBtn();
            });
        }
    }

    // Bootstrap Shell
    function init() {
        initRail();
        initDrawer();
        initRouting();
        initControls();
        updateClock();
        setInterval(updateClock, 1000);

        if (global.I18n) {
            global.I18n.applyTranslations();
        }
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }

    global.WorkspaceShell = {
        switchModule,
        openDrawer,
        closeDrawer
    };
})(window);
