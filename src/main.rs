//! Rajada — interface gráfica (egui).
//! No Windows, a versão de release não abre a janela de console.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod config;
mod engine;
mod registro;

use config::Preferencias;
use eframe::egui::{self, Color32, RichText, ViewportCommand};
use engine::{Config, Ctl, ErroDl, formatar_bytes, formatar_tempo, nome_da_url, sha_valido};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

/// Ícone em RGBA bruto (256 x 256), gerado a partir de assets/rajada.png.
const ICONE_RGBA: &[u8] = include_bytes!("../assets/icone.rgba");
const ICONE_LADO: usize = 256;

const MAX_REGISTRO: usize = 300;
/// Janela usada para calcular a velocidade (média dos últimos segundos).
const JANELA_VELOCIDADE: Duration = Duration::from_secs(4);

const ACENTO: Color32 = Color32::from_rgb(24, 120, 255);
const COR_SUCESSO: Color32 = Color32::from_rgb(46, 160, 67);
const COR_ERRO: Color32 = Color32::from_rgb(218, 54, 51);
const COR_AVISO: Color32 = Color32::from_rgb(210, 140, 30);

/// Mensagens enviadas da thread de download para a interface.
enum Evento {
    Log(String),
    Concluido(String),
    Falhou(String),
    Cancelado,
}

/// Caixas de diálogo modais.
enum Dialogo {
    Nenhum,
    Mensagem {
        titulo: String,
        texto: String,
    },
    /// O destino já existe e não há download parcial: confirmar substituição.
    Substituir,
    /// Fechar a janela com um download em andamento.
    FecharComDownload,
    Sobre,
}

/// Ação escolhida num diálogo, aplicada depois de desenhá-lo.
enum Acao {
    Fechar,
    Substituir,
    CancelarEFechar,
    ContinuarBaixando,
}

struct App {
    url: String,
    destino: String,
    conexoes: u32,
    limite_kbps: u64,
    /// Última pasta usada para salvar.
    pasta: Option<String>,
    /// Campos avançados (opcionais).
    sha256: String,
    referer: String,
    cookie: String,
    proxy: String,
    escuro: bool,
    tema_aplicado: bool,
    icone: Option<egui::TextureHandle>,
    /// URLs esperando a vez, baixadas uma após a outra.
    fila: VecDeque<String>,

    em_andamento: bool,
    ctl: Option<Ctl>,
    rx: Option<Receiver<Evento>>,
    /// Fecha a janela assim que o download cancelado terminar.
    fechar_ao_terminar: bool,
    /// Começa o próximo item da fila quando o atual termina.
    avancar_fila: bool,
    /// (instante, bytes baixados) para calcular a velocidade recente.
    amostras: VecDeque<(Instant, u64)>,
    /// Segmentos prontos do download atual, para o mapa de progresso.
    mapa: Vec<bool>,

    progresso: f32,
    indeterminado: bool,
    status: String,
    status_cor: Option<Color32>,
    detalhes: String,
    registro: Vec<String>,
    dialogo: Dialogo,
    ultimo_sucesso: Option<PathBuf>,
}

impl App {
    fn com_preferencias(prefs: Preferencias, fila: Vec<String>) -> Self {
        Self {
            url: String::new(),
            destino: String::new(),
            conexoes: prefs.conexoes,
            limite_kbps: prefs.limite_kbps,
            pasta: prefs.pasta,
            sha256: String::new(),
            referer: String::new(),
            cookie: String::new(),
            proxy: prefs.proxy.unwrap_or_default(),
            escuro: prefs.escuro,
            tema_aplicado: false,
            icone: None,
            fila: fila.into(),
            em_andamento: false,
            ctl: None,
            rx: None,
            fechar_ao_terminar: false,
            avancar_fila: false,
            amostras: VecDeque::new(),
            mapa: Vec::new(),
            progresso: 0.0,
            indeterminado: false,
            status: "Pronto para baixar.".into(),
            status_cor: None,
            detalhes: String::new(),
            registro: Vec::new(),
            dialogo: Dialogo::Nenhum,
            ultimo_sucesso: None,
        }
    }

    fn preferencias(&self) -> Preferencias {
        Preferencias {
            conexoes: self.conexoes,
            limite_kbps: self.limite_kbps,
            pasta: self.pasta.clone(),
            escuro: self.escuro,
            proxy: (!self.proxy.trim().is_empty()).then(|| self.proxy.trim().to_string()),
        }
    }

    fn salvar_prefs(&self) {
        if let Err(e) = config::salvar_em(&config::caminho_padrao(), &self.preferencias()) {
            registro::registrar(
                "WARN",
                format!("não foi possível salvar as preferências: {e}"),
            );
        }
    }

    fn persistir_fila(&self) {
        let itens: Vec<String> = self.fila.iter().cloned().collect();
        if let Err(e) = config::salvar_fila(&config::caminho_fila(), &itens) {
            registro::registrar("WARN", format!("não foi possível salvar a fila: {e}"));
        }
    }
}

/// Pasta para salvar: a última usada, se ainda existir; senão Downloads do usuário.
fn pasta_padrao(preferida: Option<&str>) -> PathBuf {
    if let Some(p) = preferida.map(PathBuf::from).filter(|p| p.is_dir()) {
        return p;
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

/// Abre uma pasta no gerenciador de arquivos do sistema.
fn abrir_pasta(pasta: &Path) {
    #[cfg(target_os = "windows")]
    let mut comando = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut comando = std::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut comando = std::process::Command::new("xdg-open");

    if let Err(e) = comando.arg(pasta).spawn() {
        registro::registrar(
            "WARN",
            format!("Não foi possível abrir a pasta {}: {e}", pasta.display()),
        );
    }
}

/// Cartão: bloco com título, usado para agrupar os campos.
fn cartao(ui: &mut egui::Ui, titulo: &str, conteudo: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(RichText::new(titulo).strong().size(14.0));
        ui.add_space(4.0);
        conteudo(ui);
    });
}

/// Mapa de segmentos: um quadrado por segmento, azul quando já foi baixado.
fn desenhar_mapa(ui: &mut egui::Ui, mapa: &[bool]) {
    let largura = ui.available_width().max(80.0);
    let passo = 9.0_f32;
    let colunas = ((largura / passo).floor() as usize).max(1);
    let linhas = mapa.len().div_ceil(colunas).max(1);
    // Limita a altura para não crescer sem fim com muitos segmentos.
    let altura = (linhas as f32 * passo).min(72.0);
    let celula_h = altura / linhas as f32;
    let celula_w = largura / colunas as f32;

    let (resposta, pintor) = ui.allocate_painter(egui::vec2(largura, altura), egui::Sense::hover());
    let origem = resposta.rect.min;
    let cinza = ui.visuals().widgets.inactive.bg_fill;
    for (i, &pronto) in mapa.iter().enumerate() {
        let (col, lin) = (i % colunas, i / colunas);
        let pos = origem + egui::vec2(col as f32 * celula_w, lin as f32 * celula_h);
        let rect = egui::Rect::from_min_size(
            pos,
            egui::vec2((celula_w - 2.0).max(1.0), (celula_h - 2.0).max(1.0)),
        );
        let cor = if pronto { ACENTO } else { cinza };
        pintor.rect_filled(rect, 2.0, cor);
    }
}

impl App {
    fn validar(&self) -> Option<String> {
        let url = self.url.trim();
        if url.is_empty() {
            return Some("Informe a URL do arquivo.".into());
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Some("A URL deve começar com http:// ou https://.".into());
        }
        let sha = self.sha256.trim();
        if !sha.is_empty() && !sha_valido(sha) {
            return Some("O SHA-256 deve ter 64 caracteres hexadecimais (ou ficar vazio).".into());
        }
        let proxy = self.proxy.trim();
        if !proxy.is_empty() && !(proxy.starts_with("http://") || proxy.starts_with("https://")) {
            return Some("O proxy deve começar com http:// ou https://.".into());
        }
        None
    }

    /// Destino efetivo: o campo, ou a pasta padrão com o nome vindo da URL.
    fn destino_efetivo(&self) -> PathBuf {
        if self.destino.trim().is_empty() {
            pasta_padrao(self.pasta.as_deref()).join(nome_da_url(self.url.trim()))
        } else {
            PathBuf::from(self.destino.trim())
        }
    }

    fn registrar(&mut self, texto: String) {
        self.registro.push(texto);
        if self.registro.len() > MAX_REGISTRO {
            let excesso = self.registro.len() - MAX_REGISTRO;
            self.registro.drain(0..excesso);
        }
    }

    fn mostrar_erro(&mut self, texto: String) {
        self.dialogo = Dialogo::Mensagem {
            titulo: "Dados inválidos".into(),
            texto,
        };
    }

    /// Valida, e pergunta antes de substituir um arquivo que já existe.
    fn pedir_inicio(&mut self) {
        if self.em_andamento {
            return;
        }
        // Sem URL no campo, começa o primeiro item da fila.
        if self.url.trim().is_empty()
            && let Some(proxima) = self.fila.pop_front()
        {
            self.url = proxima;
            self.persistir_fila();
        }
        if let Some(erro) = self.validar() {
            self.mostrar_erro(erro);
            return;
        }
        let destino = self.destino_efetivo();
        if destino.is_file() && !engine::tem_download_parcial(&destino) {
            self.dialogo = Dialogo::Substituir;
            return;
        }
        self.iniciar_agora();
    }

    /// Adiciona a URL do campo à fila e limpa o campo.
    fn adicionar_fila(&mut self) {
        if let Some(erro) = self.validar() {
            self.mostrar_erro(erro);
            return;
        }
        let url = self.url.trim().to_string();
        self.registrar(format!("Adicionado à fila: {url}"));
        self.fila.push_back(url);
        self.url.clear();
        self.persistir_fila();
    }

    fn cabecalhos(&self) -> Vec<(String, String)> {
        let mut cabs = Vec::new();
        if !self.referer.trim().is_empty() {
            cabs.push(("Referer".to_string(), self.referer.trim().to_string()));
        }
        if !self.cookie.trim().is_empty() {
            cabs.push(("Cookie".to_string(), self.cookie.trim().to_string()));
        }
        cabs
    }

    fn iniciar_agora(&mut self) {
        let url = self.url.trim().to_string();
        let destino = self.destino_efetivo();
        self.destino = destino.display().to_string();
        self.pasta = destino.parent().map(|p| p.display().to_string());
        self.salvar_prefs();

        let sha = self.sha256.trim();
        let cfg = Config {
            url: url.clone(),
            destino: destino.clone(),
            conexoes: self.conexoes.max(1),
            limite_kbps: self.limite_kbps,
            sha256: (!sha.is_empty()).then(|| sha.to_string()),
            cabecalhos: self.cabecalhos(),
            proxy: (!self.proxy.trim().is_empty()).then(|| self.proxy.trim().to_string()),
        };
        let ctl = Ctl::default();
        let (tx, rx) = mpsc::channel::<Evento>();

        let ctl_thread = ctl.clone();
        let tx_thread = tx.clone();
        thread::spawn(move || {
            let tx_log = tx_thread.clone();
            let log = move |m: String| {
                let _ = tx_log.send(Evento::Log(m));
            };
            // Um pânico no motor vira falha na tela (o hook do pânico já registra no log),
            // em vez de deixar a interface presa em "em andamento".
            let resultado = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                engine::executar(&cfg, &ctl_thread, &log)
            }))
            .unwrap_or_else(|_| Err(ErroDl::Outro("erro interno inesperado (veja o log)".into())));
            let evento = match resultado {
                Ok(()) => Evento::Concluido(cfg.destino.display().to_string()),
                Err(ErroDl::Cancelado) => Evento::Cancelado,
                Err(erro) => Evento::Falhou(erro.to_string()),
            };
            let _ = tx_thread.send(evento);
        });

        self.registrar(format!("Iniciando: {url}"));
        self.em_andamento = true;
        self.indeterminado = true;
        self.progresso = 0.0;
        self.status = "Conectando…".into();
        self.status_cor = None;
        self.detalhes.clear();
        self.ultimo_sucesso = None;
        self.amostras.clear();
        self.mapa.clear();
        self.ctl = Some(ctl);
        self.rx = Some(rx);
    }

    fn cancelar(&mut self) {
        if let Some(ctl) = &self.ctl {
            ctl.cancelar.store(true, Ordering::Relaxed);
            self.status = "Cancelando…".into();
            self.registrar("Cancelamento solicitado.".into());
        }
    }

    /// Consome eventos da thread de download.
    fn processar_eventos(&mut self, ctx: &egui::Context) {
        let mut eventos = Vec::new();
        let mut desconectou = false;
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(ev) => eventos.push(ev),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        desconectou = true;
                        break;
                    }
                }
            }
        }
        if desconectou && self.em_andamento && eventos.is_empty() {
            // A thread terminou sem avisar: trata como falha em vez de travar a tela.
            eventos.push(Evento::Falhou(
                "a transferência terminou sem resultado".into(),
            ));
        }
        for ev in eventos {
            match ev {
                Evento::Log(m) => self.registrar(m),
                Evento::Concluido(caminho) => {
                    self.finalizar();
                    self.status = "Download concluído.".into();
                    self.status_cor = Some(COR_SUCESSO);
                    self.progresso = 1.0;
                    self.ultimo_sucesso = Some(PathBuf::from(&caminho));
                    self.registrar(format!("Salvo em: {caminho}"));
                    registro::registrar(
                        "INFO",
                        format!("Interface: download concluido em {caminho}"),
                    );
                    if self.fila.is_empty() {
                        self.dialogo = Dialogo::Mensagem {
                            titulo: "Concluído".into(),
                            texto: format!("Download concluído:\n{caminho}"),
                        };
                    }
                    self.avancar_fila = true;
                }
                Evento::Falhou(msg) => {
                    self.finalizar();
                    self.status = "Falhou. Veja os detalhes no registro.".into();
                    self.status_cor = Some(COR_ERRO);
                    self.registrar(format!("ERRO: {msg}"));
                    registro::registrar("ERROR", format!("Interface: download falhou: {msg}"));
                    if self.fila.is_empty() {
                        self.dialogo = Dialogo::Mensagem {
                            titulo: "Erro no download".into(),
                            texto: msg,
                        };
                    }
                    // Um erro não para a fila: o próximo item começa.
                    self.avancar_fila = true;
                }
                Evento::Cancelado => {
                    self.finalizar();
                    self.status =
                        "Cancelado. Inicie de novo para retomar, se o servidor permitir.".into();
                    self.status_cor = Some(COR_AVISO);
                    self.registrar("Download cancelado.".into());
                    registro::registrar("INFO", "Interface: download cancelado pelo usuario");
                    // Cancelar interrompe a fila também.
                    if !self.fila.is_empty() {
                        self.registrar(format!("Fila cancelada ({} itens).", self.fila.len()));
                        self.fila.clear();
                        self.persistir_fila();
                    }
                    self.avancar_fila = false;
                }
            }
            if self.fechar_ao_terminar {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }

        if self.avancar_fila && !self.fechar_ao_terminar && !self.em_andamento {
            self.avancar_fila = false;
            if let Some(proxima) = self.fila.pop_front() {
                self.persistir_fila();
                self.url = proxima;
                self.destino.clear();
                if let Some(erro) = self.validar() {
                    self.registrar(format!("Item da fila ignorado: {erro}"));
                } else {
                    self.iniciar_agora();
                }
            }
        }
    }

    fn finalizar(&mut self) {
        self.em_andamento = false;
        self.indeterminado = false;
        self.ctl = None;
        self.rx = None;
        self.detalhes.clear();
    }

    /// Atualiza barra, velocidade, tempo restante e mapa a partir dos contadores.
    fn atualizar_progresso(&mut self) {
        let Some(ctl) = &self.ctl else { return };
        let baixado = ctl.baixado.load(Ordering::Relaxed);
        let total = ctl.total.load(Ordering::Relaxed);
        if let Ok(mapa) = ctl.mapa.lock() {
            self.mapa.clone_from(&mapa);
        }

        // Velocidade pela média dos últimos segundos, sem contar bytes que já estavam no disco.
        let agora = Instant::now();
        self.amostras.push_back((agora, baixado));
        while self.amostras.len() > 2
            && self
                .amostras
                .front()
                .is_some_and(|(t, _)| agora.duration_since(*t) > JANELA_VELOCIDADE)
        {
            self.amostras.pop_front();
        }
        let velocidade = match (self.amostras.front(), self.amostras.back()) {
            (Some(&(t0, b0)), Some(&(t1, b1))) if t1 > t0 => {
                (b1 - b0) as f64 / (t1 - t0).as_secs_f64()
            }
            _ => 0.0,
        };

        if total > 0 {
            self.indeterminado = false;
            self.progresso = (baixado as f64 / total as f64).clamp(0.0, 1.0) as f32;
            let restante = if velocidade > 0.0 {
                (total - baixado.min(total)) as f64 / velocidade
            } else {
                0.0
            };
            self.status = format!(
                "{:.1}%  —  {} de {}",
                self.progresso * 100.0,
                formatar_bytes(baixado as f64),
                formatar_bytes(total as f64)
            );
            self.detalhes = format!(
                "Velocidade: {}/s   Tempo restante: {}",
                formatar_bytes(velocidade),
                formatar_tempo(restante)
            );
        } else if baixado > 0 {
            self.indeterminado = true;
            self.status = format!("Baixado: {}", formatar_bytes(baixado as f64));
            self.detalhes = format!("Velocidade: {}/s", formatar_bytes(velocidade));
        }
    }

    fn escolher_destino(&mut self) {
        let nome = if self.url.trim().is_empty() {
            "download.bin".to_string()
        } else {
            nome_da_url(self.url.trim())
        };
        if let Some(caminho) = rfd::FileDialog::new()
            .set_directory(pasta_padrao(self.pasta.as_deref()))
            .set_file_name(&nome)
            .save_file()
        {
            self.pasta = caminho.parent().map(|p| p.display().to_string());
            self.destino = caminho.display().to_string();
        }
    }

    /// Cabeçalho: ícone, título e ações rápidas (tema, logs e sobre).
    fn desenhar_cabecalho(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(icone) = &self.icone {
                ui.add(egui::Image::new(icone).fit_to_exact_size(egui::vec2(40.0, 40.0)));
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("Rajada").strong().size(22.0));
                ui.label(RichText::new("Baixa arquivos em várias conexões ao mesmo tempo.").weak());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Sobre").clicked() {
                    self.dialogo = Dialogo::Sobre;
                }
                let rotulo = if self.escuro {
                    "Tema claro"
                } else {
                    "Tema escuro"
                };
                if ui.small_button(rotulo).clicked() {
                    self.escuro = !self.escuro;
                    self.tema_aplicado = false;
                    self.salvar_prefs();
                }
                if ui
                    .small_button("Pasta de logs")
                    .on_hover_text("Abre a pasta onde ficam os arquivos de log")
                    .clicked()
                    && let Some(log) = registro::caminho()
                    && let Some(pasta) = log.parent()
                {
                    abrir_pasta(pasta);
                }
            });
        });
        ui.add_space(6.0);
    }

    /// Cartão de download: URL, destino e botões principais.
    fn desenhar_download(&mut self, ui: &mut egui::Ui) {
        cartao(ui, "Download", |ui| {
            egui::Grid::new("grade_entrada")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("URL");
                    let resposta = ui.add(
                        egui::TextEdit::singleline(&mut self.url)
                            .desired_width(f32::INFINITY)
                            .hint_text("https://exemplo.com/arquivo.zip"),
                    );
                    if resposta.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.pedir_inicio();
                    }
                    ui.end_row();

                    ui.label("Salvar em");
                    ui.horizontal(|ui| {
                        let largura = (ui.available_width() - 110.0).max(120.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.destino)
                                .desired_width(largura)
                                .hint_text("Vazio = última pasta usada, com o nome da URL"),
                        );
                        if ui.button("Procurar…").clicked() {
                            self.escolher_destino();
                        }
                    });
                    ui.end_row();
                });

            if !self.url.trim().is_empty() && self.validar().is_none() {
                let nome = self
                    .destino_efetivo()
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(RichText::new(format!("Arquivo: {nome}")).weak().small());
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let botao_iniciar =
                    egui::Button::new(RichText::new("Iniciar").strong().color(Color32::WHITE))
                        .fill(ACENTO)
                        .min_size(egui::vec2(120.0, 32.0));
                if ui.add_enabled(!self.em_andamento, botao_iniciar).clicked() {
                    self.pedir_inicio();
                }
                if ui
                    .add_enabled(
                        self.em_andamento,
                        egui::Button::new("Cancelar").min_size(egui::vec2(100.0, 32.0)),
                    )
                    .on_hover_text("Atalho: Esc")
                    .clicked()
                {
                    self.cancelar();
                }
                if ui
                    .add_enabled(
                        !self.url.trim().is_empty(),
                        egui::Button::new("Adicionar à fila").min_size(egui::vec2(130.0, 32.0)),
                    )
                    .on_hover_text("Guarda esta URL para baixar depois, uma após a outra")
                    .clicked()
                {
                    self.adicionar_fila();
                }
                if let Some(arquivo) = self.ultimo_sucesso.clone()
                    && ui.button("Abrir pasta do arquivo").clicked()
                    && let Some(pasta) = arquivo.parent()
                {
                    abrir_pasta(pasta);
                }
            });
        });
    }

    /// Cartão de opções: conexões, limite e campos avançados.
    fn desenhar_opcoes(&mut self, ui: &mut egui::Ui) {
        cartao(ui, "Opções", |ui| {
            ui.horizontal(|ui| {
                ui.label("Conexões");
                ui.add_enabled(
                    !self.em_andamento,
                    egui::Slider::new(&mut self.conexoes, 1..=32),
                )
                .on_hover_text(
                    "Quantas partes baixar ao mesmo tempo (1 a 32). Fica salvo para a próxima vez.",
                );
            });
            ui.horizontal(|ui| {
                ui.label("Limite (KB/s)");
                ui.add_enabled(
                    !self.em_andamento,
                    egui::DragValue::new(&mut self.limite_kbps).range(0..=10_000_000),
                )
                .on_hover_text("0 = sem limite de velocidade.");
                ui.label(RichText::new("0 = sem limite").weak().small());
            });

            egui::CollapsingHeader::new("Avançado (opcional)")
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new("grade_avancado")
                        .num_columns(2)
                        .spacing([10.0, 6.0])
                        .show(ui, |ui| {
                            let campos = [
                                (
                                    "SHA-256",
                                    &mut self.sha256,
                                    "64 caracteres; confere o arquivo ao final",
                                ),
                                (
                                    "Referer",
                                    &mut self.referer,
                                    "Página de origem, se o site exigir",
                                ),
                                ("Cookie", &mut self.cookie, "nome=valor; outro=valor"),
                                ("Proxy", &mut self.proxy, "http://servidor:8080"),
                            ];
                            for (rotulo, valor, dica) in campos {
                                ui.label(rotulo);
                                ui.add_enabled(
                                    !self.em_andamento,
                                    egui::TextEdit::singleline(valor)
                                        .desired_width(f32::INFINITY)
                                        .hint_text(dica),
                                );
                                ui.end_row();
                            }
                        });
                });
        });
    }

    /// Cartão de progresso: barra, status, velocidade e mapa de segmentos.
    fn desenhar_status(&mut self, ui: &mut egui::Ui) {
        cartao(ui, "Progresso", |ui| {
            if self.indeterminado {
                ui.add(egui::ProgressBar::new(0.0).animate(true));
            } else {
                ui.add(egui::ProgressBar::new(self.progresso).show_percentage());
            }
            let texto = RichText::new(&self.status).strong();
            let texto = match self.status_cor {
                Some(cor) => texto.color(cor),
                None => texto,
            };
            ui.label(texto);
            if !self.detalhes.is_empty() {
                ui.label(RichText::new(&self.detalhes).weak());
            }
            if !self.mapa.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("Partes").weak().small());
                desenhar_mapa(ui, &self.mapa);
            }
        });
    }

    /// Cartão da fila, com remoção de itens.
    fn desenhar_fila(&mut self, ui: &mut egui::Ui) {
        if self.fila.is_empty() {
            return;
        }
        cartao(ui, &format!("Fila ({} item(ns))", self.fila.len()), |ui| {
            let mut remover = None;
            for (i, url) in self.fila.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui
                        .small_button("✕")
                        .on_hover_text("Remover da fila")
                        .clicked()
                    {
                        remover = Some(i);
                    }
                    ui.label(RichText::new(url).small());
                });
            }
            ui.horizontal(|ui| {
                if ui.small_button("Limpar fila").clicked() {
                    self.fila.clear();
                    self.persistir_fila();
                }
            });
            if let Some(i) = remover {
                self.fila.remove(i);
                self.persistir_fila();
            }
        });
    }

    /// Registro de eventos, recolhido por padrão.
    fn desenhar_registro(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Registro")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("Limpar").clicked() {
                        self.registro.clear();
                    }
                });
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(180.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for linha in &self.registro {
                            let texto = RichText::new(linha).monospace().small();
                            if linha.starts_with("ERRO") {
                                ui.label(texto.color(COR_ERRO));
                            } else {
                                ui.label(texto);
                            }
                        }
                    });
            });
    }

    /// Diálogos modais. Devolve a ação escolhida, se houver.
    fn desenhar_dialogo(&self, ctx: &egui::Context) -> Option<Acao> {
        let mut acao = None;
        match &self.dialogo {
            Dialogo::Nenhum => {}
            Dialogo::Mensagem { titulo, texto } => {
                egui::Window::new(titulo.as_str())
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(texto.as_str());
                        ui.add_space(8.0);
                        if ui.button("OK").clicked() {
                            acao = Some(Acao::Fechar);
                        }
                    });
            }
            Dialogo::Sobre => {
                egui::Window::new("Sobre o Rajada")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(RichText::new("Rajada").strong().size(18.0));
                        ui.label(format!("Versão {}", env!("CARGO_PKG_VERSION")));
                        ui.add_space(4.0);
                        ui.label("Acelerador de downloads com várias conexões.");
                        ui.label("Licença MIT.");
                        ui.add_space(8.0);
                        if ui.button("OK").clicked() {
                            acao = Some(Acao::Fechar);
                        }
                    });
            }
            Dialogo::Substituir => {
                egui::Window::new("Substituir arquivo?")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!(
                            "Já existe um arquivo com este nome:\n{}\n\nSubstituir pelo novo download?",
                            self.destino_efetivo().display()
                        ));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Substituir").clicked() {
                                acao = Some(Acao::Substituir);
                            }
                            if ui.button("Cancelar").clicked() {
                                acao = Some(Acao::Fechar);
                            }
                        });
                    });
            }
            Dialogo::FecharComDownload => {
                egui::Window::new("Download em andamento")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(
                            "Há um download em andamento. As partes já baixadas ficam salvas\n\
                             e você pode retomar depois.",
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancelar download e fechar").clicked() {
                                acao = Some(Acao::CancelarEFechar);
                            }
                            if ui.button("Continuar baixando").clicked() {
                                acao = Some(Acao::ContinuarBaixando);
                            }
                        });
                    });
            }
        }
        acao
    }

    fn aplicar_acao(&mut self, acao: Acao) {
        match acao {
            Acao::Fechar | Acao::ContinuarBaixando => self.dialogo = Dialogo::Nenhum,
            Acao::Substituir => {
                self.dialogo = Dialogo::Nenhum;
                self.iniciar_agora();
            }
            Acao::CancelarEFechar => {
                self.dialogo = Dialogo::Nenhum;
                self.fechar_ao_terminar = true;
                self.cancelar();
            }
        }
    }

    fn aplicar_tema(&mut self, ctx: &egui::Context) {
        if self.tema_aplicado {
            return;
        }
        let mut visuais = if self.escuro {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuais.selection.bg_fill = ACENTO;
        visuais.hyperlink_color = ACENTO;
        ctx.set_visuals(visuais);
        self.tema_aplicado = true;
    }
}

impl eframe::App for App {
    /// Lógica sem desenho: consome eventos e atualiza o progresso.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Fechar a janela com download ativo pede confirmação antes.
        if ctx.input(|i| i.viewport().close_requested()) && self.em_andamento {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if !matches!(self.dialogo, Dialogo::FecharComDownload) {
                self.dialogo = Dialogo::FecharComDownload;
            }
        }

        // Esc cancela o download em andamento (só quando nenhum diálogo está aberto).
        if self.em_andamento
            && matches!(self.dialogo, Dialogo::Nenhum)
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.cancelar();
        }

        self.processar_eventos(ctx);
        if self.em_andamento {
            self.atualizar_progresso();
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Desenho da janela.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.aplicar_tema(&ctx);
        if self.icone.is_none() {
            let imagem =
                egui::ColorImage::from_rgba_unmultiplied([ICONE_LADO, ICONE_LADO], ICONE_RGBA);
            self.icone = Some(ctx.load_texture("icone", imagem, egui::TextureOptions::LINEAR));
        }

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.desenhar_cabecalho(ui);
                    self.desenhar_download(ui);
                    ui.add_space(6.0);
                    self.desenhar_opcoes(ui);
                    ui.add_space(6.0);
                    self.desenhar_status(ui);
                    ui.add_space(6.0);
                    self.desenhar_fila(ui);
                    ui.add_space(6.0);
                    self.desenhar_registro(ui);
                });
        });

        if let Some(acao) = self.desenhar_dialogo(&ctx) {
            self.aplicar_acao(acao);
        }
    }
}

fn main() -> eframe::Result<()> {
    // Variável ACEL_TESTE, para diagnosticar o arraste da janela:
    //   (vazio)  -> padrão (wgpu com VSync automático)
    //   glow     -> renderizador OpenGL
    //   novsync  -> wgpu sem sincronização vertical
    let caminho_log = registro::iniciar();
    let teste = std::env::var("ACEL_TESTE")
        .unwrap_or_default()
        .to_ascii_lowercase();
    registro::registrar(
        "INFO",
        format!(
            "Iniciando Rajada v{} | SO={} arch={} | ACEL_TESTE='{}' | log={}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            teste,
            caminho_log.display()
        ),
    );

    let prefs = config::carregar_de(&config::caminho_padrao());
    let fila = config::carregar_fila(&config::caminho_fila());

    let renderer = if teste == "glow" {
        eframe::Renderer::Glow
    } else {
        eframe::Renderer::Wgpu
    };

    let mut wgpu_options = eframe::WgpuConfiguration::default();
    if teste == "novsync" {
        wgpu_options.surface.present_mode = eframe::egui_wgpu::wgpu::PresentMode::AutoNoVsync;
    }

    let icone = egui::IconData {
        rgba: ICONE_RGBA.to_vec(),
        width: ICONE_LADO as u32,
        height: ICONE_LADO as u32,
    };
    let opcoes = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([700.0, 680.0])
            .with_min_inner_size([560.0, 480.0])
            .with_title("Rajada")
            .with_icon(Arc::new(icone)),
        renderer,
        wgpu_options,
        ..Default::default()
    };

    let resultado = eframe::run_native(
        "Rajada",
        opcoes,
        Box::new(move |_cc| Ok(Box::new(App::com_preferencias(prefs, fila)))),
    );

    // No Windows, sem console, uma falha ao abrir a janela passaria despercebida.
    if let Err(erro) = &resultado {
        registro::registrar("ERROR", format!("Falha ao abrir a janela: {erro}"));
        let _ = rfd::MessageDialog::new()
            .set_title("Rajada")
            .set_description(format!(
                "Não foi possível abrir a janela.\n\n{erro}\n\nDetalhes em:\n{}",
                caminho_log.display()
            ))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    resultado
}
