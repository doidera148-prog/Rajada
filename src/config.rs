//! Preferências do usuário (conexões, limite de velocidade e última pasta),
//! guardadas em um arquivo de texto simples `chave=valor`.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Preferencias {
    pub conexoes: u32,
    pub limite_kbps: u64,
    /// Última pasta usada para salvar, se houver.
    pub pasta: Option<String>,
}

impl Default for Preferencias {
    fn default() -> Self {
        Self {
            conexoes: 8,
            limite_kbps: 0,
            pasta: None,
        }
    }
}

/// Pasta de configuração por sistema:
///   Windows: %APPDATA%\Rajada
///   macOS:   ~/Library/Application Support/Rajada
///   Linux:   $XDG_CONFIG_HOME/Rajada (ou ~/.config/Rajada)
pub fn caminho_padrao() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "windows")]
    let pasta = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or(home)
        .join("Rajada");
    #[cfg(target_os = "macos")]
    let pasta = home
        .join("Library")
        .join("Application Support")
        .join("Rajada");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let pasta = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("Rajada");
    pasta.join("config.txt")
}

/// Lê as preferências de `caminho`. Qualquer problema vira o padrão.
pub fn carregar_de(caminho: &Path) -> Preferencias {
    let mut prefs = Preferencias::default();
    let Ok(texto) = fs::read_to_string(caminho) else {
        return prefs;
    };
    for linha in texto.lines() {
        let Some((chave, valor)) = linha.split_once('=') else {
            continue;
        };
        match chave.trim() {
            "conexoes" => {
                if let Ok(n) = valor.trim().parse::<u32>() {
                    prefs.conexoes = n.clamp(1, 32);
                }
            }
            "limite_kbps" => {
                if let Ok(n) = valor.trim().parse::<u64>() {
                    prefs.limite_kbps = n.min(10_000_000);
                }
            }
            "pasta" if !valor.is_empty() => prefs.pasta = Some(valor.to_string()),
            _ => {}
        }
    }
    prefs
}

/// Grava as preferências em `caminho`, criando a pasta se preciso.
pub fn salvar_em(caminho: &Path, prefs: &Preferencias) -> std::io::Result<()> {
    if let Some(pasta) = caminho.parent() {
        fs::create_dir_all(pasta)?;
    }
    let pasta = prefs.pasta.as_deref().unwrap_or("");
    let texto = format!(
        "conexoes={}\nlimite_kbps={}\npasta={}\n",
        prefs.conexoes, prefs.limite_kbps, pasta
    );
    fs::write(caminho, texto)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ida_e_volta_e_valores_invalidos() {
        let dir = std::env::temp_dir().join(format!("acel_cfg_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let caminho = dir.join("config.txt");

        let prefs = Preferencias {
            conexoes: 12,
            limite_kbps: 500,
            pasta: Some("/tmp/x".into()),
        };
        salvar_em(&caminho, &prefs).unwrap();
        assert_eq!(carregar_de(&caminho), prefs);

        fs::write(&caminho, "conexoes=999\nlimite_kbps=abc\nlixo\n").unwrap();
        let lido = carregar_de(&caminho);
        assert_eq!(lido.conexoes, 32);
        assert_eq!(lido.limite_kbps, 0);
        assert_eq!(lido.pasta, None);

        assert_eq!(
            carregar_de(&dir.join("nao_existe.txt")),
            Preferencias::default()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
