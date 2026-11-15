# Binários compilados — Rajada

| Arquivo | Sistema | Código-fonte (commit) |
|---|---|---|
| `rajada-linux-x86_64` | Linux (x86_64) | `5e45624` (nome Rajada) |
| `rajada-windows-x86_64.exe` | Windows 10/11 (64 bits) | `d39ab97` (nome antigo, interface anterior) |
| `rajada-macos-arm64` | macOS (Apple Silicon) | `d39ab97` (nome antigo, interface anterior) |

Esta pasta é atualizada automaticamente pelo workflow **Build** (`.github/workflows/build.yml`)
a cada push para o `main` que altere o código. Não edite estes arquivos à mão.

Os binários de Windows e macOS da tabela acima ainda são da versão anterior (commit `d39ab97`)
e serão substituídos na próxima execução do workflow.

## Como usar

- **Windows:** dê dois cliques no `.exe`.
- **Linux:** `chmod +x rajada-linux-x86_64` e depois `./rajada-linux-x86_64`.
- **macOS:** `chmod +x rajada-macos-arm64` e depois `./rajada-macos-arm64`.
  Na primeira execução, o macOS pode bloquear o programa por não ser assinado. Nesse caso,
  clique com o botão direito, escolha **Abrir** e confirme.

Os logs agora ficam em uma pasta chamada **Rajada** (antiga pasta *AceleradorDownloads* não é mais usada).
