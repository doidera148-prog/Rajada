//! Motor de download: divide o arquivo em partes baixadas em paralelo
//! (HTTP Range), retoma partes parciais, aplica limite de velocidade e
//! permite cancelamento. Não depende da interface gráfica.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Tamanho dos blocos de leitura/escrita. Blocos maiores reduzem chamadas de sistema.
const CHUNK: usize = 256 * 1024;
/// Buffer usado na junção final das partes.
const BUFFER_JUNCAO: usize = 4 * 1024 * 1024;
/// Tamanho mínimo de cada parte. Partes menores não compensam o paralelismo.
const MIN_PARTE: u64 = 1024 * 1024;
const TENTATIVAS: u32 = 5;
const TIMEOUT: Duration = Duration::from_secs(15);

/// URL sem a query string: tokens de links assinados não devem ir para o log.
fn url_para_log(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// Erro do motor de download.
#[derive(Debug)]
pub enum ErroDl {
    Cancelado,
    Outro(String),
}

impl From<io::Error> for ErroDl {
    fn from(e: io::Error) -> Self {
        ErroDl::Outro(e.to_string())
    }
}

impl From<ureq::Error> for ErroDl {
    fn from(e: ureq::Error) -> Self {
        ErroDl::Outro(e.to_string())
    }
}

pub type Res<T> = Result<T, ErroDl>;

/// Parâmetros de um download.
#[derive(Clone)]
pub struct Config {
    pub url: String,
    pub destino: PathBuf,
    pub conexoes: u32,
    /// Limite em KB/s; 0 = sem limite.
    pub limite_kbps: u64,
}

/// Estado compartilhado entre a interface e o motor.
/// `baixado` e `total` são lidos pela interface a cada quadro.
#[derive(Clone, Default)]
pub struct Ctl {
    pub cancelar: Arc<AtomicBool>,
    pub baixado: Arc<AtomicU64>,
    /// 0 = tamanho desconhecido.
    pub total: Arc<AtomicU64>,
}

impl Ctl {
    fn cancelado(&self) -> bool {
        self.cancelar.load(Ordering::Relaxed)
    }
}

/// Limitador global de velocidade (compartilhado entre as threads).
struct Limitador {
    taxa: u64, // bytes por segundo; 0 = sem limite
    inicio: Instant,
    consumido: Mutex<u64>,
}

impl Limitador {
    fn new(taxa_bytes: u64) -> Self {
        Self {
            taxa: taxa_bytes,
            inicio: Instant::now(),
            consumido: Mutex::new(0),
        }
    }

    /// Espera o necessário para respeitar o limite. O sono é fatiado para que
    /// o cancelamento continue respondendo mesmo com limites muito baixos.
    fn aguardar(&self, n: usize, ctl: &Ctl) {
        if self.taxa == 0 {
            return;
        }
        let esperado = {
            let mut c = self.consumido.lock().unwrap();
            *c += n as u64;
            Duration::from_secs_f64(*c as f64 / self.taxa as f64)
        };
        let mut restante = esperado.saturating_sub(self.inicio.elapsed());
        while restante > Duration::ZERO && !ctl.cancelado() {
            let passo = restante.min(Duration::from_millis(100));
            thread::sleep(passo);
            restante -= passo;
        }
    }
}

/// Executa o download. `log` recebe mensagens de status.
/// Em caso de sucesso, o arquivo final está em `cfg.destino`.
pub fn executar(cfg: &Config, ctl: &Ctl, log: &(dyn Fn(String) + Sync)) -> Res<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(TIMEOUT)
        .timeout_read(TIMEOUT)
        .build();

    crate::registro::registrar(
        "INFO",
        format!(
            "Download iniciado: {} -> {} | conexoes={} limite={} KB/s",
            url_para_log(&cfg.url),
            cfg.destino.display(),
            cfg.conexoes,
            cfg.limite_kbps
        ),
    );

    if let Some(pasta) = cfg.destino.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(pasta)?;
    }

    log(format!("Consultando servidor: {}", cfg.url));
    let (tamanho, suporta_range, url_final) = sondar(&agent, &cfg.url)?;
    crate::registro::registrar(
        "INFO",
        format!(
            "Sondagem: tamanho={:?} range={} url_final={}",
            tamanho,
            suporta_range,
            url_para_log(&url_final)
        ),
    );
    if url_final != cfg.url {
        log(format!("Redirecionado para: {url_final}"));
    }
    ctl.total.store(tamanho.unwrap_or(0), Ordering::Relaxed);
    let limitador = Arc::new(Limitador::new(cfg.limite_kbps * 1024));

    match tamanho {
        Some(t) if suporta_range && cfg.conexoes > 1 => {
            log(format!(
                "Tamanho: {} | Conexões: {} | Range: sim",
                formatar_bytes(t as f64),
                cfg.conexoes
            ));
            crate::registro::registrar("INFO", "Modo paralelo");
            paralelo(
                &agent,
                &url_final,
                &cfg.destino,
                t,
                cfg.conexoes,
                ctl,
                &limitador,
            )?;
        }
        _ => {
            let motivo = match tamanho {
                Some(_) if !suporta_range => "servidor sem Range",
                _ => "tamanho desconhecido",
            };
            log(format!("Usando download simples ({motivo})."));
            crate::registro::registrar("INFO", format!("Modo simples: {motivo}"));
            simples(&agent, &url_final, &cfg.destino, ctl, &limitador)?;
        }
    }

    if let Some(t) = tamanho {
        let real = fs::metadata(&cfg.destino)?.len();
        if real != t {
            return Err(ErroDl::Outro(format!(
                "tamanho final {real} difere do esperado {t}"
            )));
        }
    }
    let final_bytes = fs::metadata(&cfg.destino)?.len();
    crate::registro::registrar(
        "INFO",
        format!(
            "Download concluido: {final_bytes} bytes em {}",
            cfg.destino.display()
        ),
    );
    log(format!(
        "Concluído: {} ({})",
        cfg.destino.display(),
        formatar_bytes(final_bytes as f64)
    ));
    Ok(())
}

/// Descobre tamanho, suporte a Range e a URL final (após redirecionamentos).
fn sondar(agent: &ureq::Agent, url: &str) -> Res<(Option<u64>, bool, String)> {
    match agent.head(url).call() {
        Ok(r) => {
            let tam = r
                .header("Content-Length")
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&t| t > 0);
            let aceita = r
                .header("Accept-Ranges")
                .map(|v| v.eq_ignore_ascii_case("bytes"))
                .unwrap_or(false);
            if let Some(t) = tam {
                return Ok((Some(t), aceita, r.get_url().to_string()));
            }
            crate::registro::registrar(
                "WARN",
                "HEAD sem Content-Length valido; usando GET com Range",
            );
        }
        Err(e) => {
            crate::registro::registrar("WARN", format!("HEAD falhou ({e}); usando GET com Range"));
        }
    }

    // Plano B: GET com Range 0-0 (alguns servidores não respondem bem a HEAD).
    let r = agent.get(url).set("Range", "bytes=0-0").call()?;
    let final_url = r.get_url().to_string();
    match r.status() {
        206 => {
            let total = r
                .header("Content-Range")
                .and_then(|v| v.rsplit('/').next())
                .and_then(|s| s.parse::<u64>().ok());
            Ok((total, true, final_url))
        }
        200 => {
            let tam = r
                .header("Content-Length")
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&t| t > 0);
            Ok((tam, false, final_url))
        }
        s => Err(ErroDl::Outro(format!("resposta inesperada: HTTP {s}"))),
    }
}

/// Download paralelo: N threads, cada uma com seu próprio arquivo `.partN`.
fn paralelo(
    agent: &ureq::Agent,
    url: &str,
    destino: &Path,
    tamanho: u64,
    n: u32,
    ctl: &Ctl,
    limitador: &Arc<Limitador>,
) -> Res<()> {
    // Nunca criar partes menores que MIN_PARTE nem partes vazias.
    let n = (n as u64).min((tamanho / MIN_PARTE).max(1));
    let bloco = tamanho / n;
    let partes: Vec<PathBuf> = (0..n)
        .map(|i| PathBuf::from(format!("{}.part{}", destino.display(), i)))
        .collect();

    // Partes já baixadas em execuções anteriores contam como progresso.
    let ja_baixado: u64 = partes
        .iter()
        .filter_map(|p| fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    ctl.baixado.store(ja_baixado, Ordering::Relaxed);

    let erros: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for (i, parte) in partes.iter().enumerate() {
        let i = i as u64;
        let ini = i * bloco;
        let fim = if i == n - 1 {
            tamanho - 1
        } else {
            ini + bloco - 1
        };
        let (agent, url, parte, ctl, limitador, erros) = (
            agent.clone(),
            url.to_string(),
            parte.clone(),
            ctl.clone(),
            Arc::clone(limitador),
            Arc::clone(&erros),
        );
        handles.push(thread::spawn(move || {
            baixar_parte(&agent, &url, &parte, ini, fim, &ctl, &limitador, &erros)
        }));
    }
    for h in handles {
        let _ = h.join();
    }

    // Cancelado: não juntar partes incompletas. Os .partN ficam para retomar.
    if ctl.cancelado() {
        return Err(ErroDl::Cancelado);
    }
    let erros = erros.lock().unwrap();
    if !erros.is_empty() {
        return Err(ErroDl::Outro(format!(
            "falhas no download:\n  {}\nInicie de novo para retomar.",
            erros.join("\n  ")
        )));
    }

    // Junta as partes na ordem e remove os temporários.
    crate::registro::registrar("INFO", format!("Juntando {n} partes"));
    let mut saida = BufWriter::with_capacity(BUFFER_JUNCAO, File::create(destino)?);
    for parte in &partes {
        let mut entrada = BufReader::with_capacity(BUFFER_JUNCAO, File::open(parte)?);
        io::copy(&mut entrada, &mut saida)?;
        fs::remove_file(parte)?;
    }
    saida.flush()?;
    Ok(())
}

/// Baixa bytes [ini, fim] para `parte`, com retentativas que retomam do ponto atual.
#[allow(clippy::too_many_arguments)]
fn baixar_parte(
    agent: &ureq::Agent,
    url: &str,
    parte: &Path,
    ini: u64,
    fim: u64,
    ctl: &Ctl,
    limitador: &Limitador,
    erros: &Mutex<Vec<String>>,
) {
    let nome = parte
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for tentativa in 1..=TENTATIVAS {
        if ctl.cancelado() {
            return;
        }
        match tentar_parte(agent, url, parte, ini, fim, ctl, limitador) {
            Ok(()) => {
                crate::registro::registrar("INFO", format!("{nome} concluida"));
                return;
            }
            Err(ErroDl::Cancelado) => return,
            Err(ErroDl::Outro(msg)) => {
                if tentativa == TENTATIVAS {
                    crate::registro::registrar(
                        "ERROR",
                        format!("{nome} desistiu apos {TENTATIVAS} tentativas: {msg}"),
                    );
                    erros.lock().unwrap().push(format!("{nome}: {msg}"));
                    return;
                }
                crate::registro::registrar(
                    "WARN",
                    format!(
                        "{nome} tentativa {tentativa}/{TENTATIVAS} falhou: {msg}; nova tentativa em breve"
                    ),
                );
                thread::sleep(Duration::from_secs(1u64 << tentativa.min(4)));
            }
        }
    }
}

fn tentar_parte(
    agent: &ureq::Agent,
    url: &str,
    parte: &Path,
    ini: u64,
    fim: u64,
    ctl: &Ctl,
    limitador: &Limitador,
) -> Res<()> {
    let ja_tem = fs::metadata(parte).map(|m| m.len()).unwrap_or(0);
    let pos = ini + ja_tem;
    if pos > fim {
        return Ok(()); // parte já completa
    }

    let resp = agent
        .get(url)
        .set("Range", &format!("bytes={pos}-{fim}"))
        .call()?;
    if resp.status() != 206 {
        return Err(ErroDl::Outro(format!(
            "servidor ignorou Range (HTTP {})",
            resp.status()
        )));
    }

    let mut leitor = resp.into_reader();
    let mut arq = OpenOptions::new().create(true).append(true).open(parte)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if ctl.cancelado() {
            return Err(ErroDl::Cancelado);
        }
        let n = leitor.read(&mut buf)?;
        if n == 0 {
            break;
        }
        arq.write_all(&buf[..n])?;
        limitador.aguardar(n, ctl);
        ctl.baixado.fetch_add(n as u64, Ordering::Relaxed);
    }

    // Se a conexão fechou antes do fim, a parte está incompleta: tenta de novo.
    let esperado = fim - ini + 1;
    let atual = fs::metadata(parte)?.len();
    if atual < esperado {
        return Err(ErroDl::Outro(format!(
            "conexão encerrou cedo ({atual} de {esperado} bytes)"
        )));
    }
    Ok(())
}

/// Fallback para servidores sem Range: um único GET.
fn simples(
    agent: &ureq::Agent,
    url: &str,
    destino: &Path,
    ctl: &Ctl,
    limitador: &Limitador,
) -> Res<()> {
    let resp = agent.get(url).call()?;
    let mut leitor = resp.into_reader();
    let mut arq = File::create(destino)?;
    let mut buf = vec![0u8; CHUNK];
    ctl.baixado.store(0, Ordering::Relaxed);
    loop {
        if ctl.cancelado() {
            return Err(ErroDl::Cancelado);
        }
        let n = leitor.read(&mut buf)?;
        if n == 0 {
            break;
        }
        arq.write_all(&buf[..n])?;
        limitador.aguardar(n, ctl);
        ctl.baixado.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Utilitários
// ---------------------------------------------------------------------------

pub fn formatar_bytes(n: f64) -> String {
    let mut valor = n;
    for unidade in ["B", "KB", "MB", "GB"] {
        if valor < 1024.0 {
            return format!("{valor:.1} {unidade}");
        }
        valor /= 1024.0;
    }
    format!("{valor:.1} TB")
}

pub fn formatar_tempo(segundos: f64) -> String {
    let s = segundos.max(0.0) as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Nome do arquivo a partir da URL (ignora query e fragmento).
/// O resultado é válido em Windows, Linux e macOS.
pub fn nome_da_url(url: &str) -> String {
    let semq = url.split(['?', '#']).next().unwrap_or(url);
    let ultimo = semq.rsplit('/').next().unwrap_or("");
    sanitizar_nome(&percent_decode(ultimo))
}

/// Remove caracteres proibidos no Windows, nomes reservados e nomes vazios.
fn sanitizar_nome(nome: &str) -> String {
    const PADRAO: &str = "download.bin";
    const PROIBIDOS: &str = "<>:\"/\\|?*";
    const RESERVADOS: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let limpo: String = nome
        .chars()
        .map(|c| {
            if PROIBIDOS.contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Windows não aceita espaço ou ponto no fim do nome.
    let limpo = limpo.trim().trim_end_matches('.');
    let limpo: String = limpo.chars().take(200).collect();

    if limpo.is_empty() || limpo == "." || limpo == ".." {
        return PADRAO.to_string();
    }
    let raiz = limpo.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVADOS.contains(&raiz.as_str()) {
        return format!("_{limpo}");
    }
    limpo
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// Servidor HTTP mínimo para testes. `suporta_range` controla se responde 206.
    fn servidor(dados: Arc<Vec<u8>>, suporta_range: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let dados = Arc::clone(&dados);
                thread::spawn(move || {
                    let mut leitor = BufReader::new(stream.try_clone().unwrap());
                    let mut linha = String::new();
                    if leitor.read_line(&mut linha).unwrap_or(0) == 0 {
                        return;
                    }
                    let metodo = linha.split_whitespace().next().unwrap_or("").to_string();
                    let mut range: Option<String> = None;
                    loop {
                        let mut h = String::new();
                        if leitor.read_line(&mut h).unwrap_or(0) == 0 {
                            break;
                        }
                        let h = h.trim_end();
                        if h.is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':')
                            && k.eq_ignore_ascii_case("range")
                        {
                            range = Some(v.trim().to_string());
                        }
                    }
                    let tam = dados.len();
                    let cab = |status: &str, extra: &str, len: usize| {
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {len}\r\n{extra}Connection: close\r\n\r\n"
                        )
                    };
                    let aceita = if suporta_range {
                        "Accept-Ranges: bytes\r\n"
                    } else {
                        ""
                    };
                    if metodo == "HEAD" {
                        let _ = stream.write_all(cab("200 OK", aceita, tam).as_bytes());
                        return;
                    }
                    let parcial =
                        suporta_range && range.as_deref().is_some_and(|r| r.starts_with("bytes="));
                    if parcial {
                        let spec = range.unwrap();
                        let spec = spec.trim_start_matches("bytes=");
                        let mut p = spec.split('-');
                        let ini: usize = p.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                        let fim: usize = p
                            .next()
                            .and_then(|s| if s.is_empty() { None } else { s.parse().ok() })
                            .unwrap_or(tam - 1)
                            .min(tam - 1);
                        let corpo = &dados[ini..=fim];
                        let extra = format!(
                            "Accept-Ranges: bytes\r\nContent-Range: bytes {ini}-{fim}/{tam}\r\n"
                        );
                        let _ = stream
                            .write_all(cab("206 Partial Content", &extra, corpo.len()).as_bytes());
                        let _ = stream.write_all(corpo);
                    } else {
                        let _ = stream.write_all(cab("200 OK", aceita, tam).as_bytes());
                        let _ = stream.write_all(&dados);
                    }
                });
            }
        });
        format!("http://{addr}/arquivo.bin")
    }

    /// Bytes pseudo-aleatórios determinísticos (sem dependências).
    fn dados_de_teste(n: usize) -> Arc<Vec<u8>> {
        let mut x: u32 = 12345;
        Arc::new(
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1103515245).wrapping_add(12345);
                    (x >> 16) as u8
                })
                .collect(),
        )
    }

    fn pasta_tmp(nome: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("acel_test_{}_{}", nome, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn cfg(url: String, destino: PathBuf, conexoes: u32, limite: u64) -> Config {
        Config {
            url,
            destino,
            conexoes,
            limite_kbps: limite,
        }
    }

    fn nao_loga(_: String) {}

    #[test]
    fn download_paralelo_gera_arquivo_identico() {
        let dados = dados_de_teste(3_000_003);
        let url = servidor(Arc::clone(&dados), true);
        let dir = pasta_tmp("paralelo");
        let destino = dir.join("saida.bin");
        executar(&cfg(url, destino.clone(), 4, 0), &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        let restos: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".part"))
            .collect();
        assert!(restos.is_empty(), "partes temporárias não foram removidas");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fallback_sem_range_gera_arquivo_identico() {
        let dados = dados_de_teste(500_000);
        let url = servidor(Arc::clone(&dados), false);
        let dir = pasta_tmp("simples");
        let destino = dir.join("saida.bin");
        executar(&cfg(url, destino.clone(), 8, 0), &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retoma_partes_parciais() {
        let dados = dados_de_teste(1_000_001);
        let url = servidor(Arc::clone(&dados), true);
        let dir = pasta_tmp("retoma");
        let destino = dir.join("saida.bin");
        // Simula execução anterior interrompida: metade de cada parte já gravada.
        let n = 4usize;
        let bloco = dados.len() / n;
        for i in 0..n {
            let ini = i * bloco;
            let fim = if i == n - 1 { dados.len() } else { ini + bloco };
            let metade = (fim - ini) / 2;
            fs::write(
                format!("{}.part{}", destino.display(), i),
                &dados[ini..ini + metade],
            )
            .unwrap();
        }
        executar(
            &cfg(url, destino.clone(), n as u32, 0),
            &Ctl::default(),
            &nao_loga,
        )
        .unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelamento_no_meio_nao_gera_arquivo_final() {
        let dados = dados_de_teste(3_000_000);
        let url = servidor(Arc::clone(&dados), true);
        let dir = pasta_tmp("cancela");
        let destino = dir.join("saida.bin");
        let ctl = Ctl::default();
        let ctl2 = ctl.clone();
        // 200 KB/s em 3 MB levaria ~15 s; cancelamos após 300 ms.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            ctl2.cancelar.store(true, Ordering::Relaxed);
        });
        let res = executar(&cfg(url, destino.clone(), 4, 200), &ctl, &nao_loga);
        assert!(matches!(res, Err(ErroDl::Cancelado)), "esperava Cancelado");
        assert!(!destino.exists(), "arquivo final não deveria existir");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nome_da_url_decodifica_e_ignora_query() {
        assert_eq!(
            nome_da_url("https://x.com/pasta/arquivo.zip?token=1"),
            "arquivo.zip"
        );
        assert_eq!(nome_da_url("https://x.com/a%20b.pdf#topo"), "a b.pdf");
        assert_eq!(nome_da_url("https://x.com/"), "download.bin");
    }

    #[test]
    fn cancelamento_responde_rapido_com_limite_baixo() {
        // 300 KB a 10 KB/s: sem o sono fatiado, um bloco dormiria ~25 s.
        let dados = dados_de_teste(300_000);
        let url = servidor(Arc::clone(&dados), true);
        let dir = pasta_tmp("cancela_rapido");
        let destino = dir.join("saida.bin");
        let ctl = Ctl::default();
        let ctl2 = ctl.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            ctl2.cancelar.store(true, Ordering::Relaxed);
        });
        let inicio = Instant::now();
        let res = executar(&cfg(url, destino, 4, 10), &ctl, &nao_loga);
        assert!(matches!(res, Err(ErroDl::Cancelado)));
        assert!(
            inicio.elapsed() < Duration::from_secs(3),
            "cancelamento demorou demais"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn arquivo_pequeno_com_muitas_conexoes() {
        let dados = dados_de_teste(10_000);
        let url = servidor(Arc::clone(&dados), true);
        let dir = pasta_tmp("pequeno");
        let destino = dir.join("saida.bin");
        executar(
            &cfg(url, destino.clone(), 32, 0),
            &Ctl::default(),
            &nao_loga,
        )
        .unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cria_pasta_de_destino_inexistente() {
        let dados = dados_de_teste(5_000);
        let url = servidor(Arc::clone(&dados), false);
        let dir = pasta_tmp("pasta_nova");
        let destino = dir.join("sub").join("pasta").join("saida.bin");
        executar(&cfg(url, destino.clone(), 1, 0), &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitiza_nomes_para_todos_os_sistemas() {
        assert_eq!(sanitizar_nome("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitizar_nome("CON.txt"), "_CON.txt");
        assert_eq!(sanitizar_nome("com1"), "_com1");
        assert_eq!(sanitizar_nome("..."), "download.bin");
        assert_eq!(sanitizar_nome("   "), "download.bin");
        assert_eq!(sanitizar_nome("relatorio.pdf."), "relatorio.pdf");
    }

    #[test]
    fn formatacao() {
        assert_eq!(formatar_bytes(512.0), "512.0 B");
        assert_eq!(formatar_bytes(2048.0), "2.0 KB");
        assert_eq!(formatar_tempo(75.0), "01:15");
        assert_eq!(formatar_tempo(3725.0), "1:02:05");
    }
}
