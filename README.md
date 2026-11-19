# Rajada

Programa com interface gráfica que acelera downloads dividindo o arquivo em várias
conexões paralelas. Escrito em Rust, roda no **Windows**, **Linux** e **macOS**
(Apple Silicon e Intel).

## Recursos

- **Downloads paralelos** usando HTTP Range, com de 1 a 32 conexões.
- **Retomada segura:** cada segmento concluído é salvo em disco. Se o download for
  cancelado ou interrompido, a retomada continua de onde parou, mesmo trocando o número
  de conexões. Se o arquivo mudou no servidor (ETag ou data de modificação), ele é
  baixado de novo, sem misturar versões.
- **Gravação direta:** os dados vão para `arquivo.part` e só recebem o nome final ao
  terminar, então um arquivo incompleto nunca parece pronto.
- **Limite de velocidade** configurável em KB/s.
- **Velocidade recente:** calculada sobre os últimos segundos, não sobre a média da
  execução inteira.
- **Cancelamento** que responde mesmo com limites baixos.
- **Fallback** para servidores que não aceitam Range: download simples, em uma conexão.
- **Retentativas** automáticas em cada parte, com espera crescente. Erros que não
  adianta repetir (como 404) falham na hora.
- **Confirmações:** pergunta antes de substituir um arquivo existente, e antes de fechar
  a janela com um download em andamento.
- **Preferências salvas:** conexões, limite e última pasta usada são lembrados.
- **Registro de eventos (logs)** para diagnóstico de erros, com atalho na interface.
  URLs são gravadas sem credenciais e sem query string.
- **Verificação de SHA-256** (opcional): confere o arquivo ao final e avisa se não bater.
- **Cabeçalhos personalizados** (opcional): Referer e Cookie, para sites que exigem.
- **Fila de downloads:** adicione várias URLs e elas são baixadas uma após a outra.
- **Verificação de espaço em disco** antes de começar.
- **Mark of the Web (Windows):** arquivos baixados são marcados como vindos da internet.
- **Aviso visível** se a janela não puder ser aberta (Windows, sem console).
- **Ícone** no programa, na janela e no executável do Windows.
- **Tema claro ou escuro**, escolhido no cabeçalho e lembrado.
- **Mapa de partes:** cada quadrado mostra um segmento; os concluídos ficam azuis.
- **Proxy HTTP/HTTPS** (opcional, em *Avançado*).
- **Fila salva:** os itens da fila continuam lá se você fechar o programa.
- **Verificar arquivo:** mostra o tamanho e se o servidor aceita partes, sem baixar.
- **Mensagens de erro claras:** explica falhas de rede, acesso negado e arquivo não encontrado.

## Instalação

### Opção A — baixar pronto

Baixe os binários na aba **Releases** do GitHub. Cada versão traz os três sistemas e o
arquivo `SHA256SUMS.txt`, para conferir a integridade:

| Sistema | Arquivo |
|---|---|
| Windows 10/11 | `rajada-windows-x86_64.exe` |
| Linux (x86_64) | `rajada-linux-x86_64` |
| macOS (Apple Silicon e Intel) | `rajada-macos-universal` |

Os binários não ficam versionados no repositório. Para testes, eles também aparecem como
artefatos na aba **Actions** de cada execução do CI.

### Opção B — compilar no seu computador

**Windows 10/11:** dê dois cliques em `compilar_windows.bat`.
O script instala o que faltar (Rust via winget e Visual Studio Build Tools com a
carga de trabalho C++), compila e gera `rajada.exe` na mesma pasta.
Requisitos: winget (App Installer) e conexão com a internet. O Build Tools pode
ocupar vários GB.

**Linux ou macOS:** no terminal, dentro da pasta do projeto:

```bash
./compilar_linux.sh
```

O script instala o Rust via rustup, se faltar, e gera o binário `rajada`.

## Como usar

1. Abra o programa.
2. Cole a URL do arquivo no campo **URL**.
3. Confira o nome do arquivo logo abaixo do campo **Salvar em**. Se o campo ficar vazio,
   o arquivo vai para a última pasta usada (ou *Downloads*) com o nome original.
4. Ajuste o número de **conexões** e, se quiser, o **limite de velocidade** (0 = sem limite).
5. Clique em **Iniciar**. Acompanhe a barra, a velocidade e o tempo restante.
6. Quando terminar, use **Abrir pasta do arquivo** para ver o download.

**Fila:** digite a URL e clique em **Adicionar à fila**. Repita para outras URLs e clique
em **Iniciar** (com o campo URL vazio) para baixar uma após a outra. Um erro não para a fila;
cancelar, sim. Cada item pode ser removido pelo ✕, e a fila é salva a cada alteração.

**Avançado:** em *Avançado (opcional)* você informa o SHA-256 esperado, o Referer, o Cookie
e o proxy (por exemplo `http://servidor:8080`).
**Esc** cancela o download em andamento.

**Retomar:** se o download for cancelado ou interrompido, inicie de novo com a mesma
URL e o mesmo destino. O programa continua de onde parou. Downloads de servidores que
não aceitam Range recomeçam do zero.

No registro, o botão **Limpar** apaga as mensagens exibidas (o arquivo de log não é afetado).

## Onde ficam as preferências

| Sistema | Local |
|---|---|
| Windows | `%APPDATA%\Rajada\` |
| macOS | `~/Library/Application Support/Rajada/` |
| Linux | `~/.config/Rajada/` (ou `$XDG_CONFIG_HOME`) |

Nessa pasta ficam `config.txt` (conexões, limite, pasta, tema, proxy) e `fila.txt`
(a fila de downloads).

## Registro de eventos (logs)

O botão **Pasta de logs**, no canto superior direito, abre a pasta onde ficam os logs.

Cada execução grava um arquivo de log com versão, sistema, início e fim de downloads,
tentativas que falharam e pânicos com backtrace. Quando passa de 1 MB, o arquivo é
renomeado para `rajada.log.old`.

| Sistema | Local |
|---|---|
| Windows | `%LOCALAPPDATA%\Rajada\logs\rajada.log` |
| macOS | `~/Library/Logs/Rajada/rajada.log` |
| Linux | `~/.local/state/Rajada/logs/rajada.log` (ou `$XDG_STATE_HOME`) |

## Diagnóstico de janela lenta

A variável de ambiente `ACEL_TESTE` escolhe o renderizador gráfico:

| Valor | Efeito |
|---|---|
| (vazio) | padrão: wgpu com sincronização vertical automática |
| `glow` | renderizador OpenGL |
| `novsync` | wgpu sem sincronização vertical |

- Windows (cmd): `set ACEL_TESTE=glow` e depois `rajada.exe`
- Linux/macOS: `ACEL_TESTE=glow ./rajada`

## Diagnóstico automático

O workflow **CI** (`.github/workflows/ci.yml`) roda a cada push e em cada pull request:

- **rustfmt:** verifica a formatação do código.
- **clippy:** procura bugs prováveis e trechos que podem ser simplificados, tratando avisos como erro.
- **Testes:** roda todos os testes, em Windows, Linux e macOS.
- **cargo-audit:** verifica vulnerabilidades conhecidas nas dependências.

Em push para o `main`, o mesmo workflow também **aplica as correções mecânicas**
(`cargo fmt` e `cargo clippy --fix`) e envia o resultado ao `main`, mas só se os testes
continuarem passando. Se os testes falharem, nada é enviado.

Para rodar o diagnóstico no seu computador:

```bash
./diagnostico.sh              # só relata os problemas
./diagnostico.sh --corrigir   # aplica as correções e depois testa
```

As correções automáticas cobrem formatação e simplificações que o clippy considera seguras.
Elas não substituem a revisão de regras de negócio nem de comportamento da interface.

## Desenvolvimento

```bash
cargo build --release   # gera o binário em target/release/
cargo test --all-targets
./diagnostico.sh        # diagnóstico completo (ver acima)
```

Para publicar uma versão, crie uma tag e envie. O CI cria a release com os binários:

```bash
git tag v0.4.0
git push origin v0.4.0
```

### Estrutura

```
src/
├── main.rs       interface gráfica (egui), diálogos, mapa de partes e velocidade
├── engine.rs     motor de download: segmentos, retomada, limite, cancelamento
├── config.rs     preferências e fila salvas em arquivos de texto
└── registro.rs   registro de eventos em arquivo e captura de pânicos
assets/           ícone (PNG, ICO e RGBA)
build.rs          embute o ícone e os dados de versão no .exe do Windows
```

O motor não depende da interface, então pode ser reaproveitado em outro programa
(por exemplo, uma versão de linha de comando).

## Limitações conhecidas

- A fila é sequencial: não há downloads em paralelo entre itens nem histórico salvo.
- Não suporta sites que geram o link de download via JavaScript ou exigem login por
  navegador.
- O ganho de velocidade depende do servidor aceitar HTTP Range e de a conexão ter
  banda suficiente para as conexões paralelas.
- Os binários não são assinados digitalmente: o Windows pode mostrar aviso do
  SmartScreen e o macOS pode pedir confirmação na primeira abertura.

## Licença

Distribuído sob a licença MIT. Veja o arquivo [LICENSE](LICENSE).
