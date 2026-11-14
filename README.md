# Acelerador de Downloads

Programa com interface gráfica que acelera downloads dividindo o arquivo em várias
conexões paralelas. Escrito em Rust, roda no **Windows**, **Linux** e **macOS**.

## Recursos

- **Downloads paralelos** usando HTTP Range, com de 1 a 32 conexões.
- **Retomada automática:** partes já baixadas são aproveitadas se o download for
  interrompido ou cancelado.
- **Limite de velocidade** configurável em KB/s.
- **Cancelamento** que responde mesmo com limites baixos.
- **Fallback** para servidores que não aceitam Range: download simples, em uma conexão.
- **Retentativas** automáticas em cada parte, com espera crescente.
- **Registro de eventos (logs)** para diagnóstico de erros.

## Instalação

### Opção A — baixar pronto (GitHub Actions)
A cada atualização do código, o workflow **Build** compila para os três sistemas.
Abra a aba **Actions** do repositório, escolha a execução mais recente e baixe o
artefato do seu sistema:

| Sistema | Artefato | Arquivo |
|---|---|---|
| Windows 10/11 | `acelerador-x86_64-pc-windows-msvc` | `acelerador.exe` |
| Linux (x86_64) | `acelerador-x86_64-unknown-linux-gnu` | `acelerador` |
| macOS (Apple Silicon) | `acelerador-aarch64-apple-darwin` | `acelerador` |

### Opção B — compilar no seu computador

**Windows 10/11:** dê dois cliques em `compilar_windows.bat`.
O script instala o que faltar (Rust via winget e Visual Studio Build Tools com a
carga de trabalho C++), compila e gera `acelerador.exe` na mesma pasta.
Requisitos: winget (App Installer) e conexão com a internet. O Build Tools pode
ocupar vários GB.

**Linux ou macOS:** no terminal, dentro da pasta do projeto:

```bash
./compilar_linux.sh
```

O script instala o Rust via rustup, se faltar, e gera o binário `acelerador`.

## Como usar

1. Abra o programa.
2. Cole a URL do arquivo no campo **URL do arquivo**.
3. Confira ou altere o caminho em **Salvar como**. Se ficar vazio, o arquivo vai
   para a pasta *Downloads* com o nome original.
4. Ajuste o número de **conexões** e, se quiser, o **limite de velocidade** (0 = sem limite).
5. Clique em **Iniciar**. Acompanhe a barra, a velocidade e o tempo restante.

**Retomar:** se o download for cancelado ou interrompido, inicie de novo com a mesma
URL e o mesmo destino. O programa continua de onde parou.

## Registro de eventos (logs)

Cada execução grava um arquivo de log com versão, sistema, início e fim de downloads,
tentativas que falharam e pânicos com backtrace. Quando passa de 1 MB, o arquivo é
renomeado para `acelerador.log.old`.

| Sistema | Local |
|---|---|
| Windows | `%LOCALAPPDATA%\AceleradorDownloads\logs\acelerador.log` |
| macOS | `~/Library/Logs/AceleradorDownloads/acelerador.log` |
| Linux | `~/.local/state/AceleradorDownloads/logs/acelerador.log` (ou `$XDG_STATE_HOME`) |

URLs são gravadas sem a query string, para não registrar tokens de links assinados.

## Diagnóstico de janela lenta

A variável de ambiente `ACEL_TESTE` escolhe o renderizador gráfico:

| Valor | Efeito |
|---|---|
| (vazio) | padrão: wgpu com sincronização vertical automática |
| `glow` | renderizador OpenGL |
| `novsync` | wgpu sem sincronização vertical |

- Windows (cmd): `set ACEL_TESTE=glow` e depois `acelerador.exe`
- Linux/macOS: `ACEL_TESTE=glow ./acelerador`

## Desenvolvimento

```bash
cargo build --release   # gera o binário em target/release/
cargo test --release    # roda os testes
```

### Estrutura

```
src/
├── main.rs       interface gráfica (egui)
├── engine.rs     motor de download: partes, retomada, limite, cancelamento
└── registro.rs   registro de eventos em arquivo e captura de pânicos
```

O motor não depende da interface, então pode ser reaproveitado em outro programa
(por exemplo, uma versão de linha de comando).

## Limitações conhecidas

- Não suporta sites que geram o link de download via JavaScript ou exigem login por
  navegador.
- O ganho de velocidade depende do servidor aceitar HTTP Range e de a conexão ter
  banda suficiente para as conexões paralelas.

## Licença

Distribuído sob a licença MIT. Veja o arquivo [LICENSE](LICENSE).
