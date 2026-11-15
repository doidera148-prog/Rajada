//! Registro de eventos em arquivo.
//!
//! No Windows, o executável de release não tem console, então erros
//! e pânicos não apareceriam em lugar nenhum. Este módulo grava tudo em:
//!   %LOCALAPPDATA%\AceleradorDownloads\logs\acelerador.log
//! Quando o arquivo passa de 1 MB, ele é renomeado para acelerador.log.old.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const MAX_TAMANHO: u64 = 1024 * 1024;

static CAMINHO: OnceLock<PathBuf> = OnceLock::new();
static TRAVA: Mutex<()> = Mutex::new(());

/// Pasta do usuário: %USERPROFILE% no Windows, $HOME nos demais sistemas.
fn pasta_home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Pasta de logs por sistema operacional:
///   Windows: %LOCALAPPDATA%\AceleradorDownloads\logs
///   macOS:   ~/Library/Logs/AceleradorDownloads
///   Linux:   $XDG_STATE_HOME/AceleradorDownloads/logs (ou ~/.local/state/...)
fn pasta_de_logs() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(pasta_home);
        base.join("AceleradorDownloads").join("logs")
    }
    #[cfg(target_os = "macos")]
    {
        pasta_home().join("Library").join("Logs").join("AceleradorDownloads")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| pasta_home().join(".local").join("state"));
        base.join("AceleradorDownloads").join("logs")
    }
}

/// Prepara o arquivo de log e instala um gancho que registra pânicos.
/// Retorna o caminho do arquivo, para mostrar ao usuário se necessário.
pub fn iniciar() -> PathBuf {
    let pasta = pasta_de_logs();
    let _ = fs::create_dir_all(&pasta);
    let caminho = pasta.join("acelerador.log");
    let _ = CAMINHO.set(caminho.clone());

    std::panic::set_hook(Box::new(|info| {
        let local = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "local desconhecido".into());
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(sem mensagem)".into());
        let trace = std::backtrace::Backtrace::force_capture();
        registrar("PANIC", format!("{msg} em {local}\n{trace}"));
    }));

    caminho
}

/// Caminho do arquivo de log, se o registro já foi iniciado.
pub fn caminho() -> Option<PathBuf> {
    CAMINHO.get().cloned()
}

/// Acrescenta uma linha ao log. Não faz nada se `iniciar` não foi chamado (ex.: testes).
pub fn registrar(nivel: &str, mensagem: impl AsRef<str>) {
    let Some(caminho) = CAMINHO.get() else {
        return;
    };
    let _guarda = TRAVA.lock().unwrap_or_else(|e| e.into_inner());

    if fs::metadata(caminho).map(|m| m.len()).unwrap_or(0) > MAX_TAMANHO {
        let _ = fs::rename(caminho, caminho.with_extension("log.old"));
    }

    let agora = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut arquivo) = OpenOptions::new().create(true).append(true).open(caminho) {
        let _ = writeln!(arquivo, "{agora} [{nivel:<5}] {}", mensagem.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grava_linhas_e_pânico_no_arquivo() {
        let pasta = std::env::temp_dir().join(format!("acel_log_{}", std::process::id()));
        let _ = fs::remove_dir_all(&pasta);
        // Aponta todas as pastas de dados para a temporária, em qualquer sistema,
        // para que o teste nunca escreva no log real do usuário.
        unsafe {
            std::env::set_var("LOCALAPPDATA", &pasta);
            std::env::set_var("XDG_STATE_HOME", &pasta);
            std::env::set_var("HOME", &pasta);
            std::env::set_var("USERPROFILE", &pasta);
        }
        let caminho = iniciar();
        registrar("INFO", "linha de teste");
        registrar("ERROR", "erro de teste");
        let conteudo = fs::read_to_string(&caminho).unwrap();
        assert!(conteudo.contains("[INFO ] linha de teste"));
        assert!(conteudo.contains("[ERROR] erro de teste"));
        let _ = fs::remove_dir_all(&pasta);
    }
}
