#!/usr/bin/env bash
# Compila a Rajada no Linux ou macOS.
# Instala o Rust (rustup) se faltar, compila em release e copia o binário
# "rajada" para esta pasta.
set -euo pipefail
cd "$(dirname "$0")"

echo "=== 1/3 Rust ==="
if ! command -v cargo >/dev/null 2>&1; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    fi
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust não encontrado. Instalando via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi
cargo --version

echo "=== 2/3 Compilando (as dependências são baixadas automaticamente) ==="
cargo build --release

echo "=== 3/3 Copiando binário ==="
cp target/release/rajada ./rajada
echo "PRONTO: $(pwd)/rajada"
