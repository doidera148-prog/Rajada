# Acelerador de Downloads (Rust)

Aplicativo com interface gráfica que baixa arquivos em várias conexões paralelas
(HTTP Range), retoma downloads interrompidos e permite limitar a velocidade.

## Obter o acelerador.exe

### Opção A — GitHub Actions (sem instalar nada)
1. Suba esta pasta para um repositório no GitHub.
2. Abra a aba **Actions**, escolha **Build Windows EXE** e clique em **Run workflow**.
3. Ao terminar, baixe o artefato **acelerador-windows**: ele contém `acelerador.exe`.

### Opção B — compilar no seu Windows 10/11 (o .bat instala tudo)
1. Dê dois cliques em `compilar_windows.bat`.
2. O script verifica e instala, se faltar, o **Rust** (via winget) e o **Visual Studio Build Tools**
   com a carga de trabalho C++ (via winget). Depois baixa as dependências do projeto e compila.
3. Ao final, `acelerador.exe` aparece na própria pasta.

Requisitos: winget (App Installer) e conexão com a internet. A instalação do Build Tools
pode levar bastante tempo e ocupar vários GB. Se o Rust for instalado pelo script,
pode ser preciso fechar e reabrir a janela do .bat na primeira execução.

## Uso
Abra `acelerador.exe`, cole a URL, escolha onde salvar (ou deixe o padrão, a pasta
Downloads), ajuste conexões e limite de velocidade e clique em **Iniciar**.
Se um download for cancelado ou interrompido, inicie de novo com a mesma URL e destino
para retomar do ponto em que parou.

## Testes
    cargo test --release
