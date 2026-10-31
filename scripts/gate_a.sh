#!/usr/bin/env bash
# Gate A: a falsification screen of the long-only spot swarm, on the perpetual BTCUSDT proxy.
#
# Links the monthly archives into one tape directory, writes the gate configurations from the
# classifier artifact, runs the simulations and prints the summary that the decision rule in
# docs/plan/refactor-local-kraken.md reads. It makes no network calls: download the archives
# first with `fetch_data`.
#
#   scripts/gate_a.sh [--months "02 03 04 05 06 07 08 09"] [--source DIR] [--out DIR]
#                     [--capital N] [--dry-run]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ARTIFACT="${PROJECT_ROOT}/configs/hpo_btcusdt_2026.json"

MONTHS="02 03 04 05 06 07 08 09"
SOURCE="${PROJECT_ROOT}/data/historical/BTCUSDT"
OUT="${HOME}/atsnt-gate"
CAPITAL="5000"
DRY_RUN=0

# Trades the decision run needs before its sign counts.
MIN_TRADES=50
DECISION_RUN="spot_solo_largo_salida_maker"

# Simulations, in the order they print. The last one is the reference: the artifact's own
# breakers, so it shows what the deployed configuration would have done.
RUNS=(
    control_perp_con_cortos
    spot_solo_largo_salida_maker
    spot_solo_largo_salida_taker
    referencia_maker_con_cortacircuitos
)

usage() {
    sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --months) MONTHS="$2"; shift 2 ;;
            --source) SOURCE="$2"; shift 2 ;;
            --out) OUT="$2"; shift 2 ;;
            --capital) CAPITAL="$2"; shift 2 ;;
            --dry-run) DRY_RUN=1; shift ;;
            -h | --help) usage; exit 0 ;;
            *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
        esac
    done
    local m
    for m in $MONTHS; do
        if [[ ! "$m" =~ ^(0[1-9]|1[0-2])$ ]]; then
            echo "not a month: $m (use 01 to 12)" >&2
            exit 2
        fi
    done
}

# One tape directory per month selection, so runs over different selections never mix.
tape_dir() {
    local -a months
    read -r -a months <<< "$MONTHS"
    local IFS=-
    echo "${OUT}/tape-${months[*]}"
}

archive_path() {
    echo "${SOURCE}/BTCUSDT-aggTrades-2026-$1.parquet"
}

check_archives() {
    local m missing=0
    for m in $MONTHS; do
        if [[ ! -f "$(archive_path "$m")" ]]; then
            echo "missing archive: $(archive_path "$m")" >&2
            echo "  download it with: fetch_data --symbol BTCUSDT --year 2026 --month $((10#$m))" >&2
            missing=1
        fi
    done
    return "$missing"
}

link_tape() {
    local dir m
    dir="$(tape_dir)"
    mkdir -p "$dir"
    rm -f "$dir"/BTCUSDT-aggTrades-2026-*.parquet
    for m in $MONTHS; do
        ln -s "$(realpath -m "$(archive_path "$m")")" "$dir/BTCUSDT-aggTrades-2026-$m.parquet"
    done
    echo "tape: $dir"
}

# Gate configurations, derived from the artifact the way the v4 plan describes. The gate
# variants disable both breakers (a fraction of 1 never trips), so the whole window is measured;
# the reference keeps the artifact's breakers.
write_configs() {
    mkdir -p "$OUT"
    python3 - "$ARTIFACT" "$OUT" <<'PY'
import copy
import json
import pathlib
import sys

src = json.load(open(sys.argv[1]))
out = pathlib.Path(sys.argv[2])


def write(name, allow_short, spot_costs, resting_maker_exits, gate_breakers):
    d = copy.deepcopy(src)
    d["shared_config"]["allow_short"] = allow_short
    ec = d["execution_context"]
    if spot_costs:
        ec.update(
            maker_fee_pct="0.0025",
            taker_fee_pct="0.0040",
            max_leverage="1",
            funding_rate_pct="0",
            maintenance_margin_rate="0",
        )
    ec["resting_maker_exits"] = resting_maker_exits
    if gate_breakers:
        ec.update(max_daily_drawdown_pct="1", max_monthly_drawdown_pct="1")
    (out / f"{name}.json").write_text(json.dumps(d, indent=2) + "\n")


write("control_perp_con_cortos", True, False, True, True)
write("spot_solo_largo_salida_maker", False, True, True, True)
write("spot_solo_largo_salida_taker", False, True, False, True)
write("referencia_maker_con_cortacircuitos", False, True, True, False)
PY
}

run_simulations() {
    local dir name
    dir="$(tape_dir)"
    for name in "${RUNS[@]}"; do
        echo "running $name"
        if ! cargo run --release -p backtest --bin backtest -- \
            --data "$dir" --config "$OUT/$name.json" --capital "$CAPITAL" \
            > "$OUT/$name.log" 2>&1; then
            echo "simulation failed: see $OUT/$name.log" >&2
            exit 1
        fi
    done
}

# Text after LABEL on the first line of LOG that contains it, without the separators.
field() {
    local line
    line="$(grep -F -m1 -- "$2" "$1" || true)"
    line="${line#*"$2"}"
    sed -E 's/^[:[:space:]$]+//; s/[[:space:]]+$//' <<< "$line"
}

# The decision rule of docs/plan/refactor-local-kraken.md, Fase 1.
verdict() {
    local net="$1" trades="$2"
    if [[ -z "$net" ]]; then
        echo "SIN DATOS: el log no tiene el neto"
        return
    fi
    awk -v net="$net" -v n="$trades" -v min="$MIN_TRADES" 'BEGIN {
        if (n + 0 < min) print "NO CONCLUYENTE: " n " operaciones, menos de " min;
        else if (net + 0 <= 0) print "NO PASA: neto " net " con " n " operaciones";
        else print "PASA LA PANTALLA: neto " net " con " n " operaciones; no autoriza ejecución";
    }'
}

summarize() {
    local name log net fees trades shorts rejected halt
    {
        printf '%-38s %12s %12s %12s %8s  %s\n' \
            variante neto comisiones operaciones cortas rechazos
        for name in "${RUNS[@]}"; do
            log="$OUT/$name.log"
            net="$(field "$log" "Net Profit (Real PnL)")"
            fees="$(field "$log" "Total Fees Paid (Friction)")"
            trades="$(field "$log" "Total Trades Executed")"
            shorts="$(grep -c "Short qty:" "$log" || true)"
            rejected="$(field "$log" "Rejected Signals")"
            printf '%-38s %12s %12s %12s %8s  %s\n' \
                "$name" "$net" "$fees" "$trades" "$shorts" "$rejected"
        done
        echo
        halt="$(grep -F -m1 "Monthly circuit breaker tripped" \
            "$OUT/referencia_maker_con_cortacircuitos.log" || true)"
        if [[ -n "$halt" ]]; then
            echo "referencia: cortacircuitos mensual disparado en" \
                "$(sed -E 's/.*tripped at (.*): simulation halted.*/\1/' <<< "$halt")"
        else
            echo "referencia: el cortacircuitos mensual no salta en la ventana"
        fi
        echo "decisión ($DECISION_RUN): $(verdict \
            "$(field "$OUT/$DECISION_RUN.log" "Net Profit (Real PnL)")" \
            "$(field "$OUT/$DECISION_RUN.log" "Total Trades Executed")")"
    } | tee "$OUT/summary.txt"
}

main() {
    parse_args "$@"
    if ((DRY_RUN)); then
        check_archives || exit 1
        echo "dry run: nothing is linked, written or run"
        echo "tape:    $(tape_dir)"
        local name
        for name in "${RUNS[@]}"; do
            echo "run:     $name -> $OUT/$name.log"
        done
        exit 0
    fi
    check_archives
    link_tape
    write_configs
    run_simulations
    summarize
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
