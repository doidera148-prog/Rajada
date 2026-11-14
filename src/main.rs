//! Acelerador de downloads — interface gráfica (egui).
//! No Windows, a versão de release não abre a janela de console.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod registro;

use eframe::egui;
use engine::{formatar_bytes, formatar_tempo, nome_da_url, Config, Ctl, ErroDl};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const MAX_REGISTRO: usize = 300;

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
    detalhes: String,
    registro: Vec<String>,
    popup: Option<(String, String)>,
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
            status: "Aguardando.".into(),
            detalhes: String::new(),
            registro: Vec::new(),
            popup: None,
        }
    }
}

/// Pasta Downloads do usuário (Windows: %USERPROFILE%; Linux: $HOME).
fn pasta_padrao() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        home
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
            let resultado = engine::executar(&cfg, &ctl_thread, &log);
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
        self.detalhes.clear();
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
                    self.progresso = 1.0;
                    self.registrar(format!("Salvo em: {caminho}"));
                    registro::registrar("INFO", format!("Interface: download concluido em {caminho}"));
                    self.popup = Some(("Concluído".into(), format!("Download concluído:\n{caminho}")));
                }
                Evento::Falhou(msg) => {
                    self.finalizar();
                    self.status = "Falhou.".into();
                    self.registrar(format!("ERRO: {msg}"));
                    registro::registrar("ERROR", format!("Interface: download falhou: {msg}"));
                    self.popup = Some(("Erro no download".into(), msg));
                }
                Evento::Cancelado => {
                    self.finalizar();
                    self.status = "Cancelado. As partes ficaram salvas; inicie de novo para retomar.".into();
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
            let restante = if velocidade > 0.0 { (total - baixado) / velocidade } else { 0.0 };
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
            ui.heading("Acelerador de Downloads");
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label("URL do arquivo:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.url)
                        .desired_width(f32::INFINITY)
                        .hint_text("https://exemplo.com/arquivo.zip"),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.iniciar();
                }
            });

            ui.horizontal(|ui| {
                ui.label("Salvar como:");
                ui.add(egui::TextEdit::singleline(&mut self.destino).desired_width(f32::INFINITY));
                if ui.button("Procurar…").clicked() {
                    let nome = if self.url.trim().is_empty() {
                        "download.bin".to_string()
                    } else {
                        nome_da_url(self.url.trim())
                    };
                    let inicial = pasta_padrao();
                    if let Some(caminho) = rfd::FileDialog::new()
                        .set_directory(&inicial)
                        .set_file_name(&nome)
                        .save_file()
                    {
                        self.destino = caminho.display().to_string();
                    }
                }
            });

            ui.add_space(4.0);
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label("Conexões:");
                    ui.add_enabled(
                        !self.em_andamento,
                        egui::DragValue::new(&mut self.conexoes).range(1..=32),
                    );
                    ui.add_space(20.0);
                    ui.label("Limite (KB/s, 0 = sem limite):");
                    ui.add_enabled(
                        !self.em_andamento,
                        egui::DragValue::new(&mut self.limite_kbps).range(0..=10_000_000),
                    );
                });
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.em_andamento, egui::Button::new("Iniciar"))
                    .clicked()
                {
                    self.iniciar();
                }
                if ui
                    .add_enabled(self.em_andamento, egui::Button::new("Cancelar"))
                    .clicked()
                {
                    self.cancelar();
                }
            });

            ui.separator();
            if self.indeterminado {
                ui.add(egui::ProgressBar::new(0.0).animate(true));
            } else {
                ui.add(egui::ProgressBar::new(self.progresso).show_percentage());
            }
            ui.label(&self.status);
            ui.label(&self.detalhes);

            ui.separator();
            ui.label("Registro:");
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .max_height(ui.available_height())
                .show(ui, |ui| {
                    for linha in &self.registro {
                        ui.label(linha);
                    }
                });
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
    // Variável ACEL_TESTE, só para diagnosticar o arraste da janela no Windows:
    //   (vazio)  -> padrão (wgpu com VSync automático)
    //   glow     -> renderizador OpenGL
    //   novsync  -> wgpu sem sincronização vertical
    let caminho_log = registro::iniciar();
    let teste = std::env::var("ACEL_TESTE").unwrap_or_default().to_ascii_lowercase();
    registro::registrar(
        "INFO",
        format!(
            "Iniciando acelerador v{} | SO={} arch={} | ACEL_TESTE='{}' | log={}",
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
            .with_inner_size([680.0, 520.0])
            .with_min_inner_size([560.0, 420.0])
            .with_title("Acelerador de Downloads"),
        renderer,
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native(
        "Acelerador de Downloads",
        opcoes,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}
