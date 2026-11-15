# Binários compilados

| Arquivo | Sistema | Código-fonte (commit) |
|---|---|---|
| `acelerador-linux-x86_64` | Linux (x86_64) | `e19b36a` (interface nova) |
| `acelerador-windows-x86_64.exe` | Windows 10/11 (64 bits) | `d39ab97` (versão anterior da interface) |
| `acelerador-macos-arm64` | macOS (Apple Silicon) | `d39ab97` (versão anterior da interface) |

As versões do Windows e do macOS são anteriores à interface nova e à proteção contra
pânicos. Elas serão substituídas quando o workflow **Build** gerar novos binários
(aba Actions do repositório) e eles forem colocados aqui.

## Como usar

- **Windows:** dê dois cliques no `.exe`.
- **Linux:** `chmod +x acelerador-linux-x86_64` e depois `./acelerador-linux-x86_64`.
- **macOS:** `chmod +x acelerador-macos-arm64` e depois `./acelerador-macos-arm64`.
  Na primeira execução, o macOS pode bloquear o programa por não ser assinado. Nesse caso,
  clique com o botão direito, escolha **Abrir** e confirme.
