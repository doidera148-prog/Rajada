//! Preferências do usuário (conexões, limite, última pasta, tema e proxy) e a
//! fila de downloads, guardadas em arquivos de texto simples.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Preferencias {
    pub conexoes: u32,
    pub limite_kbps: u64,
    /// Última pasta usada para salvar, se houver.
    pub pasta: Option<String>,
    /// Tema escuro (true) ou claro (false).
    pub escuro: bool,
    /// Proxy HTTP/HTTPS, se houver.
    pub proxy: Option<String>,
}

impl Default for Preferencias {
    fn default() -> Self {
        Self {
            conexoes: 8,
            limite_kbps: 0,
            pasta: None,
            escuro: true,
            proxy: None,
        }
    }
}

/// Pasta de configuração por sistema:
///   Windows: %APPDATA%\Rajada
///   macOS:   ~/Library/Application Support/Rajada
///   Linux:   $XDG_CONFIG_HOME/Rajada (ou ~/.config/Rajada)
pub fn pasta_de_config() -> PathBuf {
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
    pasta
}

pub fn caminho_padrao() -> PathBuf {
    pasta_de_config().join("config.txt")
}

pub fn caminho_fila() -> PathBuf {
    pasta_de_config().join("fila.txt")
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
        let valor = valor.trim();
        match chave.trim() {
            "conexoes" => {
                if let Ok(n) = valor.parse::<u32>() {
                    prefs.conexoes = n.clamp(1, 32);
                }
            }
            "limite_kbps" => {
                if let Ok(n) = valor.parse::<u64>() {
                    prefs.limite_kbps = n.min(10_000_000);
                }
            }
            "pasta" if !valor.is_empty() => prefs.pasta = Some(valor.to_string()),
            "escuro" => prefs.escuro = valor != "0",
            "proxy" if !valor.is_empty() => prefs.proxy = Some(valor.to_string()),
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
    let texto = format!(
        "conexoes={}\nlimite_kbps={}\npasta={}\nescuro={}\nproxy={}\n",
        prefs.conexoes,
        prefs.limite_kbps,
        prefs.pasta.as_deref().unwrap_or(""),
        if prefs.escuro { 1 } else { 0 },
        prefs.proxy.as_deref().unwrap_or("")
    );
    fs::write(caminho, texto)
}

/// Lê a fila (uma URL por linha). Linhas vazias são ignoradas.
pub fn carregar_fila(caminho: &Path) -> Vec<String> {
    fs::read_to_string(caminho)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Grava a fila, uma URL por linha.
pub fn salvar_fila(caminho: &Path, fila: &[String]) -> std::io::Result<()> {
    if let Some(pasta) = caminho.parent() {
        fs::create_dir_all(pasta)?;
    }
    if fila.is_empty() {
        let _ = fs::remove_file(caminho);
        return Ok(());
    }
    fs::write(caminho, fila.join("\n") + "\n")
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
            escuro: false,
            proxy: Some("http://p:8080".into()),
        };
        salvar_em(&caminho, &prefs).unwrap();
        assert_eq!(carregar_de(&caminho), prefs);

        fs::write(&caminho, "conexoes=999\nlimite_kbps=abc\nlixo\n").unwrap();
        let lido = carregar_de(&caminho);
        assert_eq!(lido.conexoes, 32);
        assert_eq!(lido.limite_kbps, 0);
        assert_eq!(lido.pasta, None);
        assert_eq!(lido.proxy, None);

        assert_eq!(
            carregar_de(&dir.join("nao_existe.txt")),
            Preferencias::default()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fila_ida_e_volta_ignora_lixo() {
        let dir = std::env::temp_dir().join(format!("acel_fila_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let caminho = dir.join("fila.txt");
        let fila = vec![
            "https://a.com/x.zip".to_string(),
            "http://b.com/y".to_string(),
        ];
        salvar_fila(&caminho, &fila).unwrap();
        assert_eq!(carregar_fila(&caminho), fila);
        fs::write(&caminho, "lixo\n\nhttps://c.com/z\n").unwrap();
        assert_eq!(carregar_fila(&caminho), vec!["https://c.com/z".to_string()]);
        salvar_fila(&caminho, &[]).unwrap();
        assert!(!caminho.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
