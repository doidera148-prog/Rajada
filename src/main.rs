//! Rajada — interface gráfica (egui).
//! No Windows, a versão de release não abre a janela de console.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod engine;
mod registro;

use eframe::egui::{self, Color32, RichText};
use engine::{Config, Ctl, ErroDl, formatar_bytes, formatar_tempo, nome_da_url};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const MAX_REGISTRO: usize = 300;

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

struct App {
    url: String,
    destino: String,
    conexoes: u32,
    limite_kbps: u64,

    em_andamento: bool,
    ctl: Option<Ctl>,
    rx: Option<Receiver<Evento>>,
    inicio: Instant,

    progresso: f32,
    indeterminado: bool,
    status: String,
    status_cor: Option<Color32>,
    detalhes: String,
    registro: Vec<String>,
    popup: Option<(String, String)>,
    ultimo_sucesso: Option<PathBuf>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            url: String::new(),
            destino: String::new(),
            conexoes: 8,
            limite_kbps: 0,
            em_andamento: false,
            ctl: None,
            rx: None,
            inicio: Instant::now(),
            progresso: 0.0,
            indeterminado: false,
            status: "Pronto para baixar.".into(),
            status_cor: None,
            detalhes: String::new(),
            registro: Vec::new(),
            popup: None,
            ultimo_sucesso: None,
        }
    }
}

/// Pasta Downloads do usuário (Windows: %USERPROFILE%; demais: $HOME).
fn pasta_padrao() -> PathBuf {
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

impl App {
    fn validar(&self) -> Option<String> {
        let url = self.url.trim();
        if url.is_empty() {
            return Some("Informe a URL do arquivo.".into());
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Some("A URL deve começar com http:// ou https://.".into());
        }
        None
    }

    fn registrar(&mut self, texto: String) {
        self.registro.push(texto);
        if self.registro.len() > MAX_REGISTRO {
            let excesso = self.registro.len() - MAX_REGISTRO;
            self.registro.drain(0..excesso);
        }
    }

    fn iniciar(&mut self) {
        if self.em_andamento {
            return;
        }
        if let Some(erro) = self.validar() {
            self.popup = Some(("Dados inválidos".into(), erro));
            return;
        }

        let url = self.url.trim().to_string();
        let destino = if self.destino.trim().is_empty() {
            pasta_padrao().join(nome_da_url(&url))
        } else {
            PathBuf::from(self.destino.trim())
        };
        self.destino = destino.display().to_string();

        let cfg = Config {
            url: url.clone(),
            destino: destino.clone(),
            conexoes: self.conexoes.max(1),
            limite_kbps: self.limite_kbps,
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
                Err(ErroDl::Outro(msg)) => Evento::Falhou(msg),
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
        self.ctl = Some(ctl);
        self.rx = Some(rx);
        self.inicio = Instant::now();
    }

    fn cancelar(&mut self) {
        if let Some(ctl) = &self.ctl {
            ctl.cancelar.store(true, Ordering::Relaxed);
            self.status = "Cancelando…".into();
            self.registrar("Cancelamento solicitado.".into());
        }
    }

    /// Consome eventos da thread de download.
    fn processar_eventos(&mut self) {
        let mut eventos = Vec::new();
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(ev) => eventos.push(ev),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
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
                    self.popup = Some((
                        "Concluído".into(),
                        format!("Download concluído:\n{caminho}"),
                    ));
                }
                Evento::Falhou(msg) => {
                    self.finalizar();
                    self.status = "Falhou. Veja os detalhes no registro.".into();
                    self.status_cor = Some(COR_ERRO);
                    self.registrar(format!("ERRO: {msg}"));
                    registro::registrar("ERROR", format!("Interface: download falhou: {msg}"));
                    self.popup = Some(("Erro no download".into(), msg));
                }
                Evento::Cancelado => {
                    self.finalizar();
                    self.status =
                        "Cancelado. As partes ficaram salvas; inicie de novo para retomar.".into();
                    self.status_cor = Some(COR_AVISO);
                    self.registrar("Download cancelado.".into());
                    registro::registrar("INFO", "Interface: download cancelado pelo usuario");
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

    /// Atualiza barra, velocidade e tempo restante a partir dos contadores atômicos.
    fn atualizar_progresso(&mut self) {
        let Some(ctl) = &self.ctl else { return };
        let baixado = ctl.baixado.load(Ordering::Relaxed) as f64;
        let total = ctl.total.load(Ordering::Relaxed) as f64;
        let decorrido = self.inicio.elapsed().as_secs_f64().max(1e-6);
        let velocidade = baixado / decorrido;

        if total > 0.0 {
            self.indeterminado = false;
            self.progresso = (baixado / total).clamp(0.0, 1.0) as f32;
            let restante = if velocidade > 0.0 {
                (total - baixado) / velocidade
            } else {
                0.0
            };
            self.status = format!(
                "{:.1}%  —  {} de {}",
                self.progresso * 100.0,
                formatar_bytes(baixado),
                formatar_bytes(total)
            );
            self.detalhes = format!(
                "Velocidade: {}/s   Tempo restante: {}",
                formatar_bytes(velocidade),
                formatar_tempo(restante)
            );
        } else if baixado > 0.0 {
            self.indeterminado = true;
            self.status = format!("Baixado: {}", formatar_bytes(baixado));
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
            .set_directory(pasta_padrao())
            .set_file_name(&nome)
            .save_file()
        {
            self.destino = caminho.display().to_string();
        }
    }

    /// Cabeçalho: título e atalho para a pasta de logs.
    fn desenhar_cabecalho(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Rajada");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
        ui.label(RichText::new("Baixa arquivos em várias conexões ao mesmo tempo.").weak());
        ui.add_space(8.0);
    }

    /// Campos de URL e destino.
    fn desenhar_entrada(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
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
                        self.iniciar();
                    }
                    ui.end_row();

                    ui.label("Salvar em");
                    ui.horizontal(|ui| {
                        let largura = (ui.available_width() - 120.0).max(120.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.destino)
                                .desired_width(largura)
                                .hint_text("Vazio = pasta Downloads, com o nome da URL"),
                        );
                        if ui.button("Procurar…").clicked() {
                            self.escolher_destino();
                        }
                    });
                    ui.end_row();
                });
        });
    }

    /// Conexões e limite de velocidade.
    fn desenhar_opcoes(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Conexões");
                ui.add_enabled(
                    !self.em_andamento,
                    egui::DragValue::new(&mut self.conexoes).range(1..=32),
                )
                .on_hover_text("Quantas partes baixar ao mesmo tempo (1 a 32).");
                ui.add_space(24.0);
                ui.label("Limite (KB/s)");
                ui.add_enabled(
                    !self.em_andamento,
                    egui::DragValue::new(&mut self.limite_kbps).range(0..=10_000_000),
                )
                .on_hover_text("0 = sem limite de velocidade.");
            });
        });
    }

    /// Botões de ação.
    fn desenhar_acoes(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let tamanho = egui::vec2(120.0, 30.0);
            if ui
                .add_enabled(
                    !self.em_andamento,
                    egui::Button::new(RichText::new("Iniciar").strong()).min_size(tamanho),
                )
                .clicked()
            {
                self.iniciar();
            }
            if ui
                .add_enabled(
                    self.em_andamento,
                    egui::Button::new("Cancelar").min_size(tamanho),
                )
                .clicked()
            {
                self.cancelar();
            }
            if let Some(arquivo) = self.ultimo_sucesso.clone()
                && ui.button("Abrir pasta do arquivo").clicked()
                && let Some(pasta) = arquivo.parent()
            {
                abrir_pasta(pasta);
            }
        });
    }

    /// Barra de progresso, status e detalhes.
    fn desenhar_status(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
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
        });
    }

    /// Registro de eventos, com erros em destaque.
    fn desenhar_registro(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Registro").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Limpar").clicked() {
                    self.registro.clear();
                }
            });
        });
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
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
}

impl eframe::App for App {
    /// Lógica sem desenho: consome eventos e atualiza o progresso.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.processar_eventos();
        if self.em_andamento {
            self.atualizar_progresso();
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Desenho da janela.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            self.desenhar_cabecalho(ui);
            self.desenhar_entrada(ui);
            ui.add_space(6.0);
            self.desenhar_opcoes(ui);
            ui.add_space(8.0);
            self.desenhar_acoes(ui);
            ui.add_space(8.0);
            self.desenhar_status(ui);
            ui.add_space(8.0);
            self.desenhar_registro(ui);
        });

        if let Some((titulo, texto)) = &self.popup {
            let mut fechar = false;
            egui::Window::new(titulo.as_str())
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label(texto.as_str());
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() {
                        fechar = true;
                    }
                });
            if fechar {
                self.popup = None;
            }
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

    let renderer = if teste == "glow" {
        eframe::Renderer::Glow
    } else {
        eframe::Renderer::Wgpu
    };

    let mut wgpu_options = eframe::WgpuConfiguration::default();
    if teste == "novsync" {
        wgpu_options.surface.present_mode = eframe::egui_wgpu::wgpu::PresentMode::AutoNoVsync;
    }

    let opcoes = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([680.0, 560.0])
            .with_min_inner_size([560.0, 460.0])
            .with_title("Rajada"),
        renderer,
        wgpu_options,
        ..Default::default()
    };

    let resultado = eframe::run_native(
        "Rajada",
        opcoes,
        Box::new(|_cc| Ok(Box::new(App::default()))),
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
