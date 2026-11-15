# Binários compilados — Rajada

| Arquivo | Sistema | Versão |
|---|---|---|
| `rajada-linux-x86_64` | Linux (x86_64) | 0.3.0 |
| `rajada-windows-x86_64.exe` | Windows 10/11 (64 bits) | 0.1.0 (anterior) |
| `rajada-macos-arm64` | macOS (Apple Silicon) | 0.1.0 (anterior, será removido) |

Esta pasta é atualizada automaticamente pelo workflow **CI** (`.github/workflows/ci.yml`)
a cada push para o `main` que altere o código. Não edite estes arquivos à mão.

Enquanto a próxima execução do CI não termina, o Windows e o macOS desta pasta ainda são da
versão anterior. Quando o CI publicar, o macOS passa a ser `rajada-macos-universal`
(Apple Silicon e Intel) e o `rajada-macos-arm64` é removido.

As versões publicadas com tag (`v*`) também aparecem na aba **Releases**, com o arquivo
`SHA256SUMS.txt` para conferir a integridade.

## Como usar

- **Windows:** dê dois cliques no `.exe`.
- **Linux:** `chmod +x rajada-linux-x86_64` e depois `./rajada-linux-x86_64`.
- **macOS:** `chmod +x rajada-macos-arm64` e depois `./rajada-macos-arm64`.
  Na primeira execução, o macOS pode bloquear o programa por não ser assinado. Nesse caso,
  clique com o botão direito, escolha **Abrir** e confirme.

Os logs ficam em uma pasta chamada **Rajada**. As preferências (conexões, limite e última
pasta) ficam em `config.txt` na pasta de configuração do sistema (veja o README principal).
