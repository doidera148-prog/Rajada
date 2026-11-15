//! Motor de download: baixa o arquivo em segmentos paralelos (HTTP Range),
//! grava tudo direto em `destino.part` e só renomeia ao final. Mantém um
//! arquivo de estado com os segmentos já prontos, para retomar depois de
//! cancelamento ou queda. Aplica limite de velocidade, verifica SHA-256 e
//! envia cabeçalhos personalizados. Não depende da interface.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Tamanho dos blocos de leitura da rede e de leitura de arquivos.
const CHUNK: usize = 256 * 1024;
/// Limites do tamanho de cada segmento. Segmentos pequenos permitem salvar
/// progresso com frequência; grandes demais desperdiçariam dados ao cancelar.
const SEG_MIN: u64 = 256 * 1024;
const SEG_MAX: u64 = 8 * 1024 * 1024;
/// Quantas vezes cada segmento é tentado antes de desistir.
const TENTATIVAS: u32 = 5;
/// Quantas vezes o `rename` final é repetido (antivírus pode segurar o arquivo).
const TENTATIVAS_RENAME: u32 = 10;
const TIMEOUT: Duration = Duration::from_secs(15);
const AGENTE: &str = concat!("Rajada/", env!("CARGO_PKG_VERSION"));
const VERSAO_ESTADO: &str = "1";

/// Remove credenciais (usuario:senha@) e a query string de uma URL, para o log.
pub fn redigir(url: &str) -> String {
    let sem_query = url.split(['?', '#']).next().unwrap_or(url);
    let Some(i) = sem_query.find("://") else {
        return sem_query.to_string();
    };
    let (esquema, resto) = sem_query.split_at(i + 3);
    let (autoridade, caminho) = match resto.find('/') {
        Some(j) => resto.split_at(j),
        None => (resto, ""),
    };
    let host = autoridade.rsplit('@').next().unwrap_or(autoridade);
    format!("{esquema}{host}{caminho}")
}

/// Substitui a URL completa, se aparecer numa mensagem de erro, pela versão redigida.
fn limpar(msg: &str, url: &str) -> String {
    msg.replace(url, &redigir(url))
}

/// Erro do motor de download.
#[derive(Debug)]
pub enum ErroDl {
    /// O usuário cancelou.
    Cancelado,
    /// Falha que não adianta repetir (por exemplo, 404, espaço ou SHA-256 errado).
    Permanente(String),
    /// Falha temporária: o motor tenta de novo.
    Outro(String),
    /// O arquivo mudou no servidor, ou o servidor respondeu com partes que não batem.
    Mudou,
}

impl fmt::Display for ErroDl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroDl::Cancelado => write!(f, "cancelado"),
            ErroDl::Permanente(m) | ErroDl::Outro(m) => write!(f, "{m}"),
            ErroDl::Mudou => write!(
                f,
                "o arquivo mudou no servidor (ou o servidor respondeu partes inconsistentes); \
                 o parcial foi descartado. Inicie de novo."
            ),
        }
    }
}

impl From<io::Error> for ErroDl {
    fn from(e: io::Error) -> Self {
        ErroDl::Outro(e.to_string())
    }
}

impl From<ureq::Error> for ErroDl {
    fn from(e: ureq::Error) -> Self {
        match e {
            // 408, 429 e 5xx são temporários; o resto (404, 403...) não adianta repetir.
            ureq::Error::Status(code, _) if !(code == 408 || code == 429 || code >= 500) => {
                ErroDl::Permanente(format!("HTTP {code}"))
            }
            outro => ErroDl::Outro(outro.to_string()),
        }
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
    /// SHA-256 esperado (64 caracteres hexadecimais), opcional.
    pub sha256: Option<String>,
    /// Cabeçalhos extras enviados em todas as requisições (ex.: Referer, Cookie).
    pub cabecalhos: Vec<(String, String)>,
    /// Proxy HTTP/HTTPS, por exemplo http://servidor:8080. Vazio = sem proxy.
    pub proxy: Option<String>,
}

/// Estado compartilhado entre a interface e o motor.
/// `baixado` e `total` são lidos pela interface a cada quadro.
#[derive(Clone, Default)]
pub struct Ctl {
    pub cancelar: Arc<AtomicBool>,
    pub baixado: Arc<AtomicU64>,
    /// 0 = tamanho desconhecido.
    pub total: Arc<AtomicU64>,
    /// Segmentos prontos (download segmentado), para desenhar o progresso.
    pub mapa: Arc<Mutex<Vec<bool>>>,
}

impl Ctl {
    fn cancelado(&self) -> bool {
        self.cancelar.load(Ordering::Relaxed)
    }
}

fn trava<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Se uma thread entrou em pânico segurando o lock, o dado continua utilizável.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Limitador global de velocidade (compartilhado entre as conexões).
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

    /// Espera o necessário para respeitar o limite.
    fn aguardar(&self, n: usize, ctl: &Ctl) {
        if self.taxa == 0 {
            return;
        }
        let esperado = {
            let mut c = trava(&self.consumido);
            *c += n as u64;
            Duration::from_secs_f64(*c as f64 / self.taxa as f64)
        };
        let restante = esperado.saturating_sub(self.inicio.elapsed());
        dormir_cancelavel(restante, ctl);
    }
}

/// Dorme em fatias de 100 ms para que o cancelamento responda rápido.
fn dormir_cancelavel(total: Duration, ctl: &Ctl) {
    let mut restante = total;
    while restante > Duration::ZERO && !ctl.cancelado() {
        let passo = restante.min(Duration::from_millis(100));
        thread::sleep(passo);
        restante -= passo;
    }
}

/// Dados de uma requisição de download, compartilhados pelas conexões.
/// `agent` é clonado por conexão (ele é barato de clonar).
#[derive(Clone)]
struct Ctx<'a> {
    agent: ureq::Agent,
    url: &'a str,
    /// ETag (ou Last-Modified) para If-Range; vazio se não houver.
    validador: &'a str,
    cabecalhos: &'a [(String, String)],
    /// Tamanho total do arquivo; 0 quando não é conhecido.
    tamanho: u64,
    ctl: &'a Ctl,
    limitador: &'a Limitador,
}

// ---------------------------------------------------------------------------
// Arquivos auxiliares
// ---------------------------------------------------------------------------

fn com_sufixo(destino: &Path, sufixo: &str) -> PathBuf {
    let mut s = destino.as_os_str().to_os_string();
    s.push(sufixo);
    PathBuf::from(s)
}

/// Arquivo onde o download é montado antes de receber o nome final.
fn caminho_parcial(destino: &Path) -> PathBuf {
    com_sufixo(destino, ".part")
}

/// Arquivo com a lista de segmentos já prontos.
fn caminho_estado(destino: &Path) -> PathBuf {
    com_sufixo(destino, ".part.estado")
}

/// Indica se existe um download parcial para este destino (usado pela interface).
pub fn tem_download_parcial(destino: &Path) -> bool {
    caminho_parcial(destino).exists()
}

/// Tamanho de cada segmento. Há 8 segmentos por conexão, para que o progresso
/// seja salvo com frequência e as conexões não terminem todas ao mesmo tempo.
fn tamanho_segmento(tamanho: u64, conexoes: u32) -> u64 {
    (tamanho / (conexoes.max(1) as u64 * 8)).clamp(SEG_MIN, SEG_MAX)
}

/// Intervalo [ini, fim] (inclusive) do segmento `i`.
fn intervalo(i: usize, seg: u64, tamanho: u64) -> (u64, u64) {
    let ini = i as u64 * seg;
    let fim = (ini + seg).min(tamanho) - 1;
    (ini, fim)
}

/// Estado de um download segmentado, gravado em disco a cada segmento concluído.
#[derive(Debug, Clone, PartialEq)]
struct Estado {
    url: String,
    tamanho: u64,
    /// ETag (ou Last-Modified) do arquivo; vazio se o servidor não informou.
    validador: String,
    seg: u64,
    feitos: Vec<bool>,
}

impl Estado {
    fn nova(url: &str, tamanho: u64, validador: &str, seg: u64) -> Self {
        let n = tamanho.div_ceil(seg) as usize;
        Self {
            url: url.to_string(),
            tamanho,
            validador: validador.to_string(),
            seg,
            feitos: vec![false; n],
        }
    }

    fn bytes_feitos(&self) -> u64 {
        self.feitos
            .iter()
            .enumerate()
            .filter(|(_, feito)| **feito)
            .map(|(i, _)| {
                let (ini, fim) = intervalo(i, self.seg, self.tamanho);
                fim - ini + 1
            })
            .sum()
    }

    fn para_texto(&self) -> String {
        let bits: String = self
            .feitos
            .iter()
            .map(|&f| if f { '1' } else { '0' })
            .collect();
        format!(
            "versao={VERSAO_ESTADO}\nurl={}\ntamanho={}\nvalidador={}\nseg={}\nfeitos={bits}\n",
            self.url, self.tamanho, self.validador, self.seg
        )
    }

    fn de_texto(texto: &str) -> Option<Self> {
        let (mut url, mut tamanho, mut validador, mut seg, mut feitos) =
            (None, None, String::new(), None, None);
        for linha in texto.lines() {
            let (chave, valor) = linha.split_once('=')?;
            match chave {
                "url" => url = Some(valor.to_string()),
                "tamanho" => tamanho = valor.parse::<u64>().ok(),
                "validador" => validador = valor.to_string(),
                "seg" => seg = valor.parse::<u64>().ok(),
                "feitos" => feitos = Some(valor.chars().map(|c| c == '1').collect::<Vec<bool>>()),
                _ => {}
            }
        }
        let estado = Estado {
            url: url?,
            tamanho: tamanho.filter(|&t| t > 0)?,
            validador,
            seg: seg.filter(|&s| s > 0)?,
            feitos: feitos?,
        };
        // Um estado inconsistente é descartado (o download começa de novo).
        (estado.feitos.len() as u64 == estado.tamanho.div_ceil(estado.seg)).then_some(estado)
    }
}

fn salvar_estado(caminho: &Path, estado: &Estado) -> io::Result<()> {
    let tmp = com_sufixo(caminho, ".tmp");
    fs::write(&tmp, estado.para_texto())?;
    fs::rename(&tmp, caminho)
}

fn carregar_estado(caminho: &Path) -> Option<Estado> {
    fs::read_to_string(caminho)
        .ok()
        .and_then(|t| Estado::de_texto(&t))
}

/// Grava `buf` na posição `pos` sem mexer no cursor do arquivo.
/// Assim várias conexões escrevem no mesmo arquivo ao mesmo tempo.
#[cfg(unix)]
fn escrever_em(arq: &File, buf: &[u8], pos: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(arq, buf, pos)
}

#[cfg(windows)]
fn escrever_em(arq: &File, mut buf: &[u8], mut pos: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = arq.seek_write(buf, pos)?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        buf = &buf[n..];
        pos += n as u64;
    }
    Ok(())
}

/// Renomeia com novas tentativas: no Windows, antivírus ou indexador pode
/// estar com o arquivo aberto por um instante.
fn renomear_com_tentativas(de: &Path, para: &Path) -> io::Result<()> {
    let mut ultimo = None;
    for tentativa in 1..=TENTATIVAS_RENAME {
        match fs::rename(de, para) {
            Ok(()) => return Ok(()),
            Err(e) => {
                ultimo = Some(e);
                thread::sleep(Duration::from_millis(200 * u64::from(tentativa)));
            }
        }
    }
    Err(ultimo.unwrap_or_else(|| io::Error::other("falha ao renomear")))
}

/// Confere se há espaço livre para `necessario` bytes na pasta do destino.
/// Se não for possível medir o espaço, não bloqueia o download.
fn verificar_espaco(destino: &Path, necessario: u64) -> Res<()> {
    let pasta = destino
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    match fs2::available_space(pasta) {
        Ok(disponivel) if disponivel < necessario => Err(ErroDl::Permanente(format!(
            "espaço insuficiente: precisa de {} e há {} livres",
            formatar_bytes(necessario as f64),
            formatar_bytes(disponivel as f64)
        ))),
        _ => Ok(()),
    }
}

/// Marca o arquivo como vindo da internet (Windows), para o sistema avisar antes de abrir.
#[cfg(windows)]
fn marcar_origem_web(caminho: &Path) {
    let mut s = caminho.as_os_str().to_os_string();
    s.push(":Zone.Identifier");
    let _ = fs::write(PathBuf::from(s), "[ZoneTransfer]\r\nZoneId=3\r\n");
}

#[cfg(not(windows))]
fn marcar_origem_web(_caminho: &Path) {}

/// Confere se o texto é um SHA-256 válido (64 caracteres hexadecimais).
pub fn sha_valido(texto: &str) -> bool {
    let t = texto.trim();
    t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())
}

fn sha256_arquivo(caminho: &Path) -> io::Result<String> {
    let mut arq = File::open(caminho)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = arq.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Interpreta `Content-Range: bytes a-b/total`. O total pode ser `*` (desconhecido).
fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let resto = v.trim().strip_prefix("bytes ")?;
    let (intervalo, total) = resto.split_once('/')?;
    let (a, b) = intervalo.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?, total))
}

/// Aplica os cabeçalhos personalizados a uma requisição.
fn com_cabecalhos(req: ureq::Request, cabecalhos: &[(String, String)]) -> ureq::Request {
    cabecalhos.iter().fold(req, |r, (k, v)| r.set(k, v))
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

/// Executa o download. `log` recebe mensagens de status.
/// Em caso de sucesso, o arquivo final está em `cfg.destino`.
pub fn executar(cfg: &Config, ctl: &Ctl, log: &(dyn Fn(String) + Sync)) -> Res<()> {
    executar_interno(cfg, ctl, log).map_err(|e| match e {
        ErroDl::Outro(m) => ErroDl::Outro(limpar(&m, &cfg.url)),
        ErroDl::Permanente(m) => ErroDl::Permanente(limpar(&m, &cfg.url)),
        outro => outro,
    })
}

fn executar_interno(cfg: &Config, ctl: &Ctl, log: &(dyn Fn(String) + Sync)) -> Res<()> {
    if let Some(esperado) = &cfg.sha256
        && !sha_valido(esperado)
    {
        return Err(ErroDl::Permanente(
            "SHA-256 inválido: informe 64 caracteres hexadecimais".into(),
        ));
    }

    let mut construtor = ureq::AgentBuilder::new()
        .timeout_connect(TIMEOUT)
        .timeout_read(TIMEOUT)
        .user_agent(AGENTE);
    if let Some(endereco) = cfg.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
        let proxy = ureq::Proxy::new(endereco.trim())
            .map_err(|e| ErroDl::Permanente(format!("proxy inválido: {e}")))?;
        construtor = construtor.proxy(proxy);
    }
    let agent = construtor.build();

    crate::registro::registrar(
        "INFO",
        format!(
            "Download iniciado: {} -> {} | conexoes={} limite={} KB/s",
            redigir(&cfg.url),
            cfg.destino.display(),
            cfg.conexoes,
            cfg.limite_kbps
        ),
    );

    if let Some(pasta) = cfg.destino.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(pasta)?;
    }
    trava(&ctl.mapa).clear();

    log(format!("Consultando servidor: {}", redigir(&cfg.url)));
    let sonda = sondar(&agent, &cfg.url, &cfg.cabecalhos)?;
    crate::registro::registrar(
        "INFO",
        format!(
            "Sondagem: tamanho={:?} range={} url_final={}",
            sonda.tamanho,
            sonda.range,
            redigir(&sonda.url_final)
        ),
    );
    if sonda.url_final != cfg.url {
        log(format!("Redirecionado para: {}", redigir(&sonda.url_final)));
    }
    ctl.total
        .store(sonda.tamanho.unwrap_or(0), Ordering::Relaxed);
    let limitador = Limitador::new(cfg.limite_kbps * 1024);

    match sonda.tamanho {
        Some(t) if sonda.range => {
            log(format!(
                "Tamanho: {} | Conexões: {} | Range: sim",
                formatar_bytes(t as f64),
                cfg.conexoes
            ));
            crate::registro::registrar("INFO", "Modo segmentado");
            let ctx = Ctx {
                agent,
                url: &sonda.url_final,
                validador: &sonda.validador,
                cabecalhos: &cfg.cabecalhos,
                tamanho: t,
                ctl,
                limitador: &limitador,
            };
            baixar_segmentado(cfg, &ctx)?;
        }
        tamanho => {
            let motivo = match tamanho {
                Some(_) => "servidor sem Range",
                None => "tamanho desconhecido",
            };
            log(format!("Usando download simples ({motivo})."));
            crate::registro::registrar("INFO", format!("Modo simples: {motivo}"));
            if let Some(t) = tamanho {
                verificar_espaco(&cfg.destino, t)?;
            }
            let ctx = Ctx {
                agent,
                url: &sonda.url_final,
                validador: "",
                cabecalhos: &cfg.cabecalhos,
                tamanho: tamanho.unwrap_or(0),
                ctl,
                limitador: &limitador,
            };
            baixar_simples(&ctx, &cfg.destino)?;
        }
    }

    let final_bytes = fs::metadata(&cfg.destino)?.len();
    if let Some(t) = sonda.tamanho
        && final_bytes != t
    {
        return Err(ErroDl::Outro(format!(
            "tamanho final {final_bytes} difere do esperado {t}"
        )));
    }

    if let Some(esperado) = &cfg.sha256 {
        log("Conferindo SHA-256…".into());
        let obtido = sha256_arquivo(&cfg.destino)?;
        if !obtido.eq_ignore_ascii_case(esperado.trim()) {
            return Err(ErroDl::Permanente(format!(
                "SHA-256 não confere (esperado {esperado}, obtido {obtido}). \
                 O arquivo foi mantido, mas pode estar corrompido."
            )));
        }
        crate::registro::registrar("INFO", "SHA-256 confere");
    }

    marcar_origem_web(&cfg.destino);
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

/// Resultado da sondagem inicial do servidor.
struct Sonda {
    tamanho: Option<u64>,
    range: bool,
    url_final: String,
    /// ETag (ou Last-Modified) para detectar se o arquivo mudou.
    validador: String,
}

fn validador_de(r: &ureq::Response) -> String {
    // ETag fraca (W/) não serve para If-Range; usa Last-Modified nesse caso.
    if let Some(etag) = r.header("ETag").filter(|e| !e.starts_with("W/")) {
        return etag.to_string();
    }
    r.header("Last-Modified").unwrap_or("").to_string()
}

/// Descobre tamanho, suporte a Range, URL final (após redirecionamentos) e validador.
fn sondar(agent: &ureq::Agent, url: &str, cabecalhos: &[(String, String)]) -> Res<Sonda> {
    let mut tamanho_head = None;
    let mut url_head = None;
    let mut validador_head = String::new();
    match com_cabecalhos(agent.head(url), cabecalhos).call() {
        Ok(r) => {
            tamanho_head = r
                .header("Content-Length")
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&t| t > 0);
            let aceita = r
                .header("Accept-Ranges")
                .is_some_and(|v| v.eq_ignore_ascii_case("bytes"));
            validador_head = validador_de(&r);
            url_head = Some(r.get_url().to_string());
            if tamanho_head.is_some() && aceita {
                return Ok(Sonda {
                    tamanho: tamanho_head,
                    range: true,
                    url_final: r.get_url().to_string(),
                    validador: validador_head,
                });
            }
        }
        Err(e) => {
            crate::registro::registrar(
                "WARN",
                format!(
                    "HEAD falhou ({}); testando Range com GET",
                    limpar(&e.to_string(), url)
                ),
            );
        }
    }

    // Plano B: GET com Range 0-0. Alguns servidores não anunciam Accept-Ranges
    // mas aceitam Range; por isso testamos de fato.
    let r = com_cabecalhos(agent.get(url), cabecalhos)
        .set("Range", "bytes=0-0")
        .call()?;
    let url_final = r.get_url().to_string();
    let validador = if validador_head.is_empty() {
        validador_de(&r)
    } else {
        validador_head
    };
    match r.status() {
        206 => {
            let total = r
                .header("Content-Range")
                .and_then(parse_content_range)
                .and_then(|(_, _, t)| t);
            Ok(Sonda {
                tamanho: total.or(tamanho_head),
                range: true,
                url_final,
                validador,
            })
        }
        200 => Ok(Sonda {
            tamanho: tamanho_head,
            range: false,
            url_final: url_head.unwrap_or(url_final),
            validador,
        }),
        s => Err(ErroDl::Outro(format!("resposta inesperada: HTTP {s}"))),
    }
}

/// Download segmentado e retomável. Os bytes vão direto para `destino.part`
/// e o estado é salvo a cada segmento concluído.
fn baixar_segmentado(cfg: &Config, ctx: &Ctx) -> Res<()> {
    let tamanho = ctx.tamanho;
    let parcial = caminho_parcial(&cfg.destino);
    let arquivo_estado = caminho_estado(&cfg.destino);

    let retomado = carregar_estado(&arquivo_estado).filter(|e| {
        e.url == ctx.url
            && e.tamanho == tamanho
            && e.validador == ctx.validador
            && fs::metadata(&parcial).is_ok_and(|m| m.len() == tamanho)
    });
    // Só precisa de espaço para o que ainda falta.
    let ja_feito = retomado.as_ref().map_or(0, Estado::bytes_feitos);
    verificar_espaco(&cfg.destino, tamanho - ja_feito)?;

    let estado = match retomado {
        Some(e) => {
            let prontos = e.feitos.iter().filter(|&&f| f).count();
            crate::registro::registrar(
                "INFO",
                format!(
                    "Retomando: {prontos} de {} segmentos já prontos",
                    e.feitos.len()
                ),
            );
            e
        }
        None => {
            let _ = fs::remove_file(&arquivo_estado);
            File::create(&parcial)?.set_len(tamanho)?;
            Estado::nova(
                ctx.url,
                tamanho,
                ctx.validador,
                tamanho_segmento(tamanho, cfg.conexoes),
            )
        }
    };

    ctx.ctl
        .baixado
        .store(estado.bytes_feitos(), Ordering::Relaxed);
    *trava(&ctx.ctl.mapa) = estado.feitos.clone();
    let pendentes: Vec<usize> = estado
        .feitos
        .iter()
        .enumerate()
        .filter(|(_, feito)| !**feito)
        .map(|(i, _)| i)
        .collect();

    let resultado = if pendentes.is_empty() {
        Ok(())
    } else {
        let arq = OpenOptions::new().write(true).open(&parcial)?;
        let comp = Compartilhado {
            estado: Mutex::new(estado.clone()),
            arq: &arq,
            estado_path: arquivo_estado.clone(),
            seg: estado.seg,
            tamanho,
            pendentes: pendentes.clone(),
            proximo: AtomicUsize::new(0),
            erros: Mutex::new(Vec::new()),
            mudou: AtomicBool::new(false),
        };
        let n_threads = (cfg.conexoes as usize).clamp(1, pendentes.len());
        thread::scope(|s| {
            let mut handles = Vec::new();
            for _ in 0..n_threads {
                let mine = ctx.clone();
                let c = &comp;
                handles.push(s.spawn(move || trabalhador(c, &mine)));
            }
            for h in handles {
                if h.join().is_err() {
                    trava(&comp.erros)
                        .push("uma conexão falhou inesperadamente (veja o log)".into());
                }
            }
        });

        let erros = trava(&comp.erros).clone();
        if ctx.ctl.cancelado() {
            Err(ErroDl::Cancelado)
        } else if comp.mudou.load(Ordering::Relaxed) {
            Err(ErroDl::Mudou)
        } else if !erros.is_empty() {
            Err(ErroDl::Outro(format!(
                "falhas no download:\n  {}\nInicie de novo para retomar.",
                erros.join("\n  ")
            )))
        } else {
            Ok(())
        }
    };

    match resultado {
        Err(ErroDl::Mudou) => {
            // O parcial mistura versões do arquivo: descarta tudo.
            let _ = fs::remove_file(&parcial);
            let _ = fs::remove_file(&arquivo_estado);
            return Err(ErroDl::Mudou);
        }
        Err(e) => return Err(e),
        Ok(()) => {}
    }

    if fs::metadata(&parcial)?.len() != tamanho {
        return Err(ErroDl::Outro("tamanho do arquivo parcial incorreto".into()));
    }
    renomear_com_tentativas(&parcial, &cfg.destino)?;
    let _ = fs::remove_file(&arquivo_estado);
    Ok(())
}

/// Dados compartilhados pelas conexões de um mesmo download.
struct Compartilhado<'a> {
    estado: Mutex<Estado>,
    arq: &'a File,
    estado_path: PathBuf,
    seg: u64,
    tamanho: u64,
    /// Índices dos segmentos que faltam, e o próximo a pegar.
    pendentes: Vec<usize>,
    proximo: AtomicUsize,
    erros: Mutex<Vec<String>>,
    mudou: AtomicBool,
}

/// Cada conexão pega segmentos pendentes até acabarem, cancelarem ou o arquivo mudar.
fn trabalhador(c: &Compartilhado, ctx: &Ctx) {
    loop {
        if ctx.ctl.cancelado() || c.mudou.load(Ordering::Relaxed) {
            return;
        }
        let k = c.proximo.fetch_add(1, Ordering::Relaxed);
        let Some(&i) = c.pendentes.get(k) else {
            return;
        };
        let (ini, fim) = intervalo(i, c.seg, c.tamanho);
        match baixar_segmento(ctx, c.arq, ini, fim) {
            Ok(()) => {
                // Os bytes precisam estar no disco antes de o estado dizer que estão prontos.
                if let Err(e) = c.arq.sync_data() {
                    let msg = format!("erro ao gravar no disco: {e}");
                    crate::registro::registrar("ERROR", format!("parte {} falhou: {msg}", i + 1));
                    trava(&c.erros).push(format!("parte {}: {msg}", i + 1));
                    continue;
                }
                if let Some(marca) = trava(&ctx.ctl.mapa).get_mut(i) {
                    *marca = true;
                }
                let mut estado = trava(&c.estado);
                estado.feitos[i] = true;
                if let Err(e) = salvar_estado(&c.estado_path, &estado) {
                    crate::registro::registrar(
                        "WARN",
                        format!("não foi possível salvar o estado: {e}"),
                    );
                }
            }
            Err(ErroDl::Cancelado) => return,
            Err(ErroDl::Mudou) => {
                c.mudou.store(true, Ordering::Relaxed);
                return;
            }
            Err(e) => {
                let msg = limpar(&e.to_string(), ctx.url);
                crate::registro::registrar("ERROR", format!("parte {} falhou: {msg}", i + 1));
                trava(&c.erros).push(format!("parte {}: {msg}", i + 1));
            }
        }
    }
}

/// Baixa bytes [ini, fim] com retentativas. Cada tentativa regrava o segmento
/// inteiro na posição certa, então uma tentativa interrompida não corrompe nada.
fn baixar_segmento(ctx: &Ctx, arq: &File, ini: u64, fim: u64) -> Res<()> {
    for tentativa in 1..=TENTATIVAS {
        if ctx.ctl.cancelado() {
            return Err(ErroDl::Cancelado);
        }
        let mut escritos = 0u64;
        let resultado = tentar_segmento(ctx, arq, ini, fim, &mut escritos);
        // Bytes de uma tentativa que falhou não contam como progresso.
        if resultado.is_err() {
            ctx.ctl.baixado.fetch_sub(escritos, Ordering::Relaxed);
        }
        match resultado {
            Ok(()) => return Ok(()),
            Err(ErroDl::Outro(m)) if tentativa < TENTATIVAS => {
                crate::registro::registrar(
                    "WARN",
                    format!(
                        "tentativa {tentativa}/{TENTATIVAS} falhou: {m}; nova tentativa em breve"
                    ),
                );
                dormir_cancelavel(Duration::from_secs(1u64 << tentativa.min(4)), ctx.ctl);
            }
            Err(ErroDl::Outro(m)) => {
                return Err(ErroDl::Outro(format!(
                    "desistiu após {TENTATIVAS} tentativas: {m}"
                )));
            }
            Err(outro) => return Err(outro),
        }
    }
    Err(ErroDl::Outro("sem resultado".into()))
}

fn tentar_segmento(ctx: &Ctx, arq: &File, ini: u64, fim: u64, escritos: &mut u64) -> Res<()> {
    let mut req = com_cabecalhos(ctx.agent.get(ctx.url), ctx.cabecalhos)
        .set("Range", &format!("bytes={ini}-{fim}"));
    if !ctx.validador.is_empty() {
        // Se o arquivo mudou, o servidor responde com o arquivo inteiro (200).
        req = req.set("If-Range", ctx.validador);
    }
    let resp = req.call()?;
    match resp.status() {
        206 => {
            // Alguns servidores ignoram If-Range: confere se a parte é a pedida.
            let Some((inicio_resp, _, total_resp)) =
                resp.header("Content-Range").and_then(parse_content_range)
            else {
                return Err(ErroDl::Permanente(
                    "resposta 206 sem Content-Range válido".into(),
                ));
            };
            let total_diferente = total_resp.is_some_and(|t| ctx.tamanho > 0 && t != ctx.tamanho);
            if inicio_resp != ini || total_diferente {
                return Err(ErroDl::Mudou);
            }
        }
        200 if !ctx.validador.is_empty() => return Err(ErroDl::Mudou),
        200 => {
            return Err(ErroDl::Permanente(
                "o servidor ignorou o pedido de parte (sem Range)".into(),
            ));
        }
        s => return Err(ErroDl::Outro(format!("resposta inesperada: HTTP {s}"))),
    }

    let mut leitor = resp.into_reader();
    let mut buf = vec![0u8; CHUNK];
    let mut pos = ini;
    while pos <= fim {
        if ctx.ctl.cancelado() {
            return Err(ErroDl::Cancelado);
        }
        let n = leitor.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // Não grava nada além do fim pedido, mesmo que o servidor mande mais.
        let restante = (fim + 1 - pos) as usize;
        let n = n.min(restante);
        escrever_em(arq, &buf[..n], pos)
            .map_err(|e| ErroDl::Permanente(format!("erro ao gravar no disco: {e}")))?;
        pos += n as u64;
        *escritos += n as u64;
        ctx.limitador.aguardar(n, ctx.ctl);
        ctx.ctl.baixado.fetch_add(n as u64, Ordering::Relaxed);
    }

    if pos != fim + 1 {
        return Err(ErroDl::Outro(format!(
            "conexão encerrou cedo ({} de {} bytes)",
            pos - ini,
            fim - ini + 1
        )));
    }
    Ok(())
}

/// Download sem Range: um único GET, gravado em `destino.part`.
/// Não é retomável; se falhar ou for cancelado, o parcial é apagado.
fn baixar_simples(ctx: &Ctx, destino: &Path) -> Res<()> {
    let parcial = caminho_parcial(destino);
    let resultado = gravar_simples(ctx, &parcial);
    if resultado.is_err() {
        let _ = fs::remove_file(&parcial);
        return resultado;
    }
    renomear_com_tentativas(&parcial, destino)?;
    Ok(())
}

fn gravar_simples(ctx: &Ctx, parcial: &Path) -> Res<()> {
    let resp = com_cabecalhos(ctx.agent.get(ctx.url), ctx.cabecalhos).call()?;
    let mut leitor = resp.into_reader();
    let mut arq = File::create(parcial)?;
    let mut buf = vec![0u8; CHUNK];
    ctx.ctl.baixado.store(0, Ordering::Relaxed);
    loop {
        if ctx.ctl.cancelado() {
            return Err(ErroDl::Cancelado);
        }
        let n = leitor.read(&mut buf)?;
        if n == 0 {
            break;
        }
        arq.write_all(&buf[..n])?;
        ctx.limitador.aguardar(n, ctx.ctl);
        ctx.ctl.baixado.fetch_add(n as u64, Ordering::Relaxed);
    }
    arq.sync_all()?;
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
    use std::io::{BufRead, BufReader};
    use std::net::{TcpListener, TcpStream};

    /// Conteúdo servido pelo servidor de teste. Mudar `etag`/`dados` simula
    /// um arquivo que foi atualizado no servidor.
    #[derive(Clone)]
    struct Conteudo {
        dados: Arc<Vec<u8>>,
        etag: String,
        /// Responde 403 se o cabeçalho `x-rajada-teste: sim` não vier.
        exige_cabecalho: bool,
        /// Informa um Content-Range que começa um byte depois do pedido.
        range_deslocado: bool,
    }

    /// Servidor HTTP mínimo para testes. Retorna a URL do arquivo.
    /// Com `range = false` nunca responde 206. Caminhos com "404" respondem 404.
    fn servidor(conteudo: Arc<Mutex<Conteudo>>, range: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let conteudo = Arc::clone(&conteudo);
                thread::spawn(move || atender(stream, &conteudo, range));
            }
        });
        format!("http://{addr}/arquivo.bin")
    }

    fn atender(mut stream: TcpStream, conteudo: &Mutex<Conteudo>, range: bool) {
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        let mut leitor = BufReader::new(clone);
        let mut linha = String::new();
        if leitor.read_line(&mut linha).unwrap_or(0) == 0 {
            return;
        }
        let mut partes = linha.split_whitespace();
        let metodo = partes.next().unwrap_or("").to_string();
        let caminho = partes.next().unwrap_or("").to_string();

        let mut range_pedido: Option<String> = None;
        let mut if_range: Option<String> = None;
        let mut teste_ok = false;
        loop {
            let mut h = String::new();
            if leitor.read_line(&mut h).unwrap_or(0) == 0 {
                break;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                let v = v.trim();
                match k.trim().to_ascii_lowercase().as_str() {
                    "range" => range_pedido = Some(v.to_string()),
                    "if-range" => if_range = Some(v.to_string()),
                    "x-rajada-teste" => teste_ok = v == "sim",
                    _ => {}
                }
            }
        }

        let snap = conteudo.lock().unwrap().clone();
        let tam = snap.dados.len();
        if caminho.contains("404") {
            responder(&mut stream, "404 Not Found", "", &[]);
            return;
        }
        if snap.exige_cabecalho && !teste_ok {
            responder(&mut stream, "403 Forbidden", "", &[]);
            return;
        }
        let comuns = format!(
            "ETag: {}\r\nAccept-Ranges: {}\r\n",
            snap.etag,
            if range { "bytes" } else { "none" }
        );
        if metodo == "HEAD" {
            responder(
                &mut stream,
                "200 OK",
                &format!("Content-Length: {tam}\r\n{comuns}"),
                &[],
            );
            return;
        }

        // Interpreta "bytes=ini-fim" (fim opcional).
        let pedido = range_pedido
            .as_deref()
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|spec| {
                let (a, b) = spec.split_once('-')?;
                let ini: usize = a.parse().ok()?;
                let fim: usize = if b.is_empty() {
                    tam.checked_sub(1)?
                } else {
                    b.parse().ok()?
                };
                (ini < tam).then_some((ini, fim.min(tam - 1)))
            });
        let if_range_ok = if_range.as_ref().is_none_or(|ir| *ir == snap.etag);

        match pedido {
            Some((ini, fim)) if range && if_range_ok => {
                let corpo = &snap.dados[ini..=fim];
                let inicio_informado = if snap.range_deslocado { ini + 1 } else { ini };
                let cab = format!(
                    "Content-Length: {}\r\nContent-Range: bytes {inicio_informado}-{fim}/{tam}\r\n{comuns}",
                    corpo.len()
                );
                responder(&mut stream, "206 Partial Content", &cab, corpo);
            }
            _ => {
                responder(
                    &mut stream,
                    "200 OK",
                    &format!("Content-Length: {tam}\r\n{comuns}"),
                    &snap.dados,
                );
            }
        }
    }

    fn responder(stream: &mut TcpStream, status: &str, cabecalhos: &str, corpo: &[u8]) {
        let _ = stream.write_all(
            format!("HTTP/1.1 {status}\r\n{cabecalhos}Connection: close\r\n\r\n").as_bytes(),
        );
        let _ = stream.write_all(corpo);
    }

    fn conteudo_de(dados: Arc<Vec<u8>>, etag: &str) -> Arc<Mutex<Conteudo>> {
        Arc::new(Mutex::new(Conteudo {
            dados,
            etag: etag.to_string(),
            exige_cabecalho: false,
            range_deslocado: false,
        }))
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
            sha256: None,
            cabecalhos: Vec::new(),
            proxy: None,
        }
    }

    fn nao_loga(_: String) {}

    /// Cancela o download quando `ctl.baixado` chega a `bytes`.
    fn cancelar_ao_chegar_em(ctl: Ctl, bytes: u64) {
        thread::spawn(move || {
            while ctl.baixado.load(Ordering::Relaxed) < bytes {
                thread::sleep(Duration::from_millis(10));
            }
            ctl.cancelar.store(true, Ordering::Relaxed);
        });
    }

    fn sobras(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".part") || n.contains(".tmp"))
            .collect()
    }

    #[test]
    fn download_paralelo_gera_arquivo_identico() {
        let dados = dados_de_teste(3_000_003);
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), true);
        let dir = pasta_tmp("paralelo");
        let destino = dir.join("saida.bin");
        executar(&cfg(url, destino.clone(), 4, 0), &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        assert!(sobras(&dir).is_empty(), "restaram arquivos temporários");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fallback_sem_range_gera_arquivo_identico() {
        let dados = dados_de_teste(500_000);
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), false);
        let dir = pasta_tmp("simples");
        let destino = dir.join("saida.bin");
        executar(&cfg(url, destino.clone(), 8, 0), &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        assert!(sobras(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retoma_com_outro_numero_de_conexoes() {
        let dados = dados_de_teste(3_000_000);
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), true);
        let dir = pasta_tmp("retoma");
        let destino = dir.join("saida.bin");

        // Primeira execução: interrompida depois de alguns segmentos prontos.
        let ctl = Ctl::default();
        cancelar_ao_chegar_em(ctl.clone(), 2_500_000);
        let res = executar(&cfg(url.clone(), destino.clone(), 8, 1000), &ctl, &nao_loga);
        assert!(
            matches!(res, Err(ErroDl::Cancelado)),
            "esperava cancelamento"
        );
        assert!(!destino.exists(), "não deve existir arquivo final");
        assert!(
            caminho_estado(&destino).exists(),
            "estado deveria ter sido salvo"
        );

        // Segunda execução: com outro número de conexões, deve retomar e acertar.
        executar(&cfg(url, destino.clone(), 3, 0), &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        assert!(sobras(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn arquivo_alterado_no_servidor_nao_mistura_versoes() {
        let dados_v1 = dados_de_teste(3_000_000);
        let dados_v2 = Arc::new(
            dados_de_teste(3_000_000)
                .iter()
                .map(|b| b.wrapping_add(7))
                .collect::<Vec<u8>>(),
        );
        let conteudo = conteudo_de(Arc::clone(&dados_v1), "\"v1\"");
        let url = servidor(Arc::clone(&conteudo), true);
        let dir = pasta_tmp("mudou");
        let destino = dir.join("saida.bin");

        let ctl = Ctl::default();
        cancelar_ao_chegar_em(ctl.clone(), 2_500_000);
        let res = executar(&cfg(url.clone(), destino.clone(), 8, 1000), &ctl, &nao_loga);
        assert!(matches!(res, Err(ErroDl::Cancelado)));

        // O arquivo muda no servidor entre as duas execuções.
        *conteudo.lock().unwrap() = Conteudo {
            dados: Arc::clone(&dados_v2),
            etag: "\"v2\"".into(),
            exige_cabecalho: false,
            range_deslocado: false,
        };
        executar(&cfg(url, destino.clone(), 8, 0), &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados_v2, "conteúdo misturado");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_range_inconsistente_e_detectado() {
        // Servidor que ignora If-Range e responde com outro deslocamento.
        let dados = dados_de_teste(1_000_000);
        let conteudo = conteudo_de(Arc::clone(&dados), "\"v1\"");
        conteudo.lock().unwrap().range_deslocado = true;
        let url = servidor(conteudo, true);
        let dir = pasta_tmp("content_range");
        let destino = dir.join("saida.bin");
        let res = executar(&cfg(url, destino.clone(), 4, 0), &Ctl::default(), &nao_loga);
        assert!(matches!(res, Err(ErroDl::Mudou)), "esperava Mudou: {res:?}");
        assert!(
            !destino.exists(),
            "não deve entregar arquivo com partes trocadas"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_confere_e_detecta_erro() {
        let dados = dados_de_teste(200_000);
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), true);
        let dir = pasta_tmp("sha");
        let dest = dir.join("ok.bin");
        let esperado = format!("{:x}", Sha256::digest(dados.as_slice()));

        let mut c = cfg(url.clone(), dest.clone(), 2, 0);
        c.sha256 = Some(esperado.clone());
        executar(&c, &Ctl::default(), &nao_loga).unwrap();

        let mut ruim = cfg(url, dir.join("ruim.bin"), 2, 0);
        ruim.sha256 = Some("0".repeat(64));
        let res = executar(&ruim, &Ctl::default(), &nao_loga);
        assert!(
            matches!(res, Err(ErroDl::Permanente(_))),
            "esperava erro de SHA: {res:?}"
        );

        let mut invalido = cfg("http://127.0.0.1:1/x".into(), dir.join("x.bin"), 1, 0);
        invalido.sha256 = Some("abc".into());
        assert!(matches!(
            executar(&invalido, &Ctl::default(), &nao_loga),
            Err(ErroDl::Permanente(_))
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cabecalhos_personalizados_sao_enviados() {
        let dados = dados_de_teste(50_000);
        let conteudo = conteudo_de(Arc::clone(&dados), "\"v1\"");
        conteudo.lock().unwrap().exige_cabecalho = true;
        let url = servidor(conteudo, true);
        let dir = pasta_tmp("cabecalhos");

        // Sem o cabeçalho, o servidor recusa.
        let sem = executar(
            &cfg(url.clone(), dir.join("sem.bin"), 2, 0),
            &Ctl::default(),
            &nao_loga,
        );
        assert!(sem.is_err());

        let mut com = cfg(url, dir.join("com.bin"), 2, 0);
        com.cabecalhos = vec![("x-rajada-teste".into(), "sim".into())];
        executar(&com, &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(dir.join("com.bin")).unwrap(), *dados);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelamento_no_meio_nao_gera_arquivo_final() {
        let dados = dados_de_teste(3_000_000);
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), true);
        let dir = pasta_tmp("cancela");
        let destino = dir.join("saida.bin");
        let ctl = Ctl::default();
        // 200 KB/s em 3 MB levaria ~15 s; cancelamos após 300 ms.
        cancelar_ao_chegar_em(ctl.clone(), 1);
        let res = executar(&cfg(url, destino.clone(), 4, 200), &ctl, &nao_loga);
        assert!(matches!(res, Err(ErroDl::Cancelado)), "esperava Cancelado");
        assert!(!destino.exists(), "arquivo final não deveria existir");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelamento_simples_nao_deixa_parcial() {
        let dados = dados_de_teste(3_000_000);
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), false);
        let dir = pasta_tmp("cancela_simples");
        let destino = dir.join("saida.bin");
        let ctl = Ctl::default();
        cancelar_ao_chegar_em(ctl.clone(), 1);
        let res = executar(&cfg(url, destino.clone(), 1, 200), &ctl, &nao_loga);
        assert!(matches!(res, Err(ErroDl::Cancelado)));
        assert!(!destino.exists());
        assert!(sobras(&dir).is_empty(), "parcial não foi apagado");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelamento_responde_rapido_com_limite_baixo() {
        // 300 KB a 10 KB/s: sem o sono fatiado, um bloco dormiria ~25 s.
        let dados = dados_de_teste(300_000);
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), true);
        let dir = pasta_tmp("cancela_rapido");
        let destino = dir.join("saida.bin");
        let ctl = Ctl::default();
        cancelar_ao_chegar_em(ctl.clone(), 1);
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
    fn erro_404_nao_e_repetido() {
        let dados = dados_de_teste(10_000);
        let url =
            servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), true).replace("arquivo.bin", "404");
        let dir = pasta_tmp("404");
        let destino = dir.join("saida.bin");
        let inicio = Instant::now();
        let res = executar(&cfg(url, destino, 4, 0), &Ctl::default(), &nao_loga);
        assert!(
            matches!(res, Err(ErroDl::Permanente(_))),
            "esperava erro permanente: {res:?}"
        );
        assert!(inicio.elapsed() < Duration::from_secs(3), "repetiu um 404");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn espaco_insuficiente_e_recusado_antes_de_comecar() {
        let dir = pasta_tmp("espaco");
        let res = verificar_espaco(&dir.join("x.bin"), u64::MAX / 2);
        assert!(matches!(res, Err(ErroDl::Permanente(_))));
        assert!(verificar_espaco(&dir.join("x.bin"), 1).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn arquivo_pequeno_com_muitas_conexoes() {
        let dados = dados_de_teste(10_000);
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), true);
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
        let url = servidor(conteudo_de(Arc::clone(&dados), "\"v1\""), false);
        let dir = pasta_tmp("pasta_nova");
        let destino = dir.join("sub").join("pasta").join("saida.bin");
        executar(&cfg(url, destino.clone(), 1, 0), &Ctl::default(), &nao_loga).unwrap();
        assert_eq!(fs::read(&destino).unwrap(), *dados);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn redige_credenciais_e_query() {
        assert_eq!(
            redigir("https://usuario:senha@exemplo.com/pasta/arq.zip?token=abc#x"),
            "https://exemplo.com/pasta/arq.zip"
        );
        assert_eq!(redigir("https://exemplo.com"), "https://exemplo.com");
    }

    #[test]
    fn estado_ida_e_volta() {
        let mut e = Estado::nova("https://a/b", 1000, "\"v\"", 300);
        assert_eq!(e.feitos.len(), 4);
        e.feitos[1] = true;
        let lido = Estado::de_texto(&e.para_texto()).unwrap();
        assert_eq!(lido, e);
        assert_eq!(lido.bytes_feitos(), 300);
        // Estado inconsistente é descartado.
        assert!(Estado::de_texto("url=x\ntamanho=1000\nseg=300\nfeitos=1\n").is_none());
    }

    #[test]
    fn content_range_e_sha_validos() {
        assert_eq!(
            parse_content_range("bytes 0-99/1000"),
            Some((0, 99, Some(1000)))
        );
        assert_eq!(parse_content_range("bytes 0-99/*"), Some((0, 99, None)));
        assert_eq!(parse_content_range("bytes */1000"), None);
        assert!(sha_valido(&"a".repeat(64)));
        assert!(!sha_valido(&"g".repeat(64)));
        assert!(!sha_valido("abc"));
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
