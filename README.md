# Acelerador de Downloads (Rust)

Aplicativo com interface gráfica que baixa arquivos em várias conexões paralelas
(HTTP Range), retoma downloads interrompidos e permite limitar a velocidade.
Funciona no Windows, Linux e macOS.

## Obter o binário

### Opção A — GitHub Actions (sem instalar nada)
A cada push, o workflow **Build** compila para Windows, Linux e macOS.
Na aba **Actions**, abra a execução mais recente e baixe o artefato do seu sistema:
- `acelerador-x86_64-pc-windows-msvc` (contém `acelerador.exe`)
- `acelerador-x86_64-unknown-linux-gnu` (contém `acelerador`)
- `acelerador-aarch64-apple-darwin` (contém `acelerador`, para Macs Apple Silicon)

### Opção B — compilar no seu computador

**Windows 10/11:** dê dois cliques em `compilar_windows.bat`.
O script instala o Rust (via winget) e o Visual Studio Build Tools com a carga de
trabalho C++, se faltarem, depois compila. O `acelerador.exe` aparece na pasta.
Requisitos: winget (App Installer) e internet. O Build Tools pode ocupar vários GB.

**Linux ou macOS:** no terminal, dentro da pasta do projeto, rode:

    ./compilar_linux.sh

O script instala o Rust via rustup, se faltar, compila e gera o binário `acelerador`.

## Uso
Abra o programa, cole a URL, escolha onde salvar (ou deixe o padrão, a pasta
Downloads), ajuste conexões e limite de velocidade e clique em **Iniciar**.
Se um download for cancelado ou interrompido, inicie de novo com a mesma URL e destino
para retomar do ponto em que parou.

## Registro de eventos (logs)
Cada execução grava um log com versão, sistema, início e fim de downloads, tentativas
que falharam e pânicos. Onde fica:
- Windows: `%LOCALAPPDATA%\AceleradorDownloads\logs\acelerador.log`
- macOS: `~/Library/Logs/AceleradorDownloads/acelerador.log`
- Linux: `~/.local/state/AceleradorDownloads/logs/acelerador.log`
  (ou `$XDG_STATE_HOME`, se estiver definido)

URLs são gravadas sem a query string, para não registrar tokens de links assinados.

## Variável de diagnóstico
`ACEL_TESTE` escolhe o renderizador gráfico, para investigar lentidão de janela:
- `glow`: renderizador OpenGL
- `novsync`: wgpu sem sincronização vertical

No Windows: `set ACEL_TESTE=glow` e depois `acelerador.exe`.
No Linux/macOS: `ACEL_TESTE=glow ./acelerador`.

## Testes
    cargo test --release
