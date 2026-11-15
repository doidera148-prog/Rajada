#!/usr/bin/env bash
# ============================================================
#  Diagnóstico da Rajada no seu computador.
#  Roda as mesmas verificações do workflow "Diagnóstico e correção automática":
#    1) formatação (rustfmt)
#    2) lints (clippy)
#    3) testes
#    4) vulnerabilidades nas dependências (cargo-audit, se instalado)
#
#  Uso:
#    ./diagnostico.sh            só relata os problemas
#    ./diagnostico.sh --corrigir aplica rustfmt e clippy --fix, depois testa
# ============================================================
set -u
cd "$(dirname "$0")" || exit 1

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo não encontrado. Instale o Rust em https://rustup.rs"
    exit 1
fi

falhas=0
passo() { echo; echo "=== $1 ==="; }

if [ "${1:-}" = "--corrigir" ]; then
    passo "Correções automáticas"
    cargo clippy --fix --allow-dirty --all-targets || true
    cargo fmt
fi

passo "1/4 Formatação (rustfmt)"
cargo fmt --check || { echo "Use ./diagnostico.sh --corrigir para formatar."; falhas=$((falhas + 1)); }

passo "2/4 Lints (clippy)"
cargo clippy --all-targets -- -D warnings || falhas=$((falhas + 1))

passo "3/4 Testes"
cargo test --all-targets || falhas=$((falhas + 1))

passo "4/4 Vulnerabilidades nas dependências"
if command -v cargo-audit >/dev/null 2>&1; then
    cargo audit || falhas=$((falhas + 1))
else
    echo "cargo-audit não instalado (opcional). Para instalar: cargo install cargo-audit"
fi

echo
if [ "$falhas" -eq 0 ]; then
    echo "OK: nenhum problema encontrado."
else
    echo "Diagnóstico terminou com $falhas problema(s). Veja as mensagens acima."
fi
exit "$falhas"
