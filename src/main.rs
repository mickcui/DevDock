#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_os = "windows"))]
compile_error!("DevDock only supports Windows");

use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use egui_extras::{Column, TableBuilder};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use ureq::ResponseExt;

const GITHUB_LATEST_RELEASE: &str = "https://api.github.com/repos/mickcui/DevDock/releases/latest";
const GITHUB_LATEST_PAGE: &str = "https://github.com/mickcui/DevDock/releases/latest";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("DevDock - WSLC管理工具")
        .with_inner_size([1100.0, 700.0])
        .with_min_inner_size([800.0, 500.0]);
    if let Ok(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "DevDock",
        options,
        Box::new(|cc| Ok(Box::new(DevDock::new(cc)))),
    )
}

fn app_icon() -> Result<egui::IconData, String> {
    let image = egui_extras::image::load_svg_bytes_with_size(
        include_bytes!("../assets/logo.svg"),
        egui::SizeHint::Size {
            width: 256,
            height: 256,
            maintain_aspect_ratio: true,
        },
        &Default::default(),
    )?;
    let rgba = image
        .pixels
        .iter()
        .flat_map(|color| color.to_srgba_unmultiplied())
        .collect();
    Ok(egui::IconData {
        rgba,
        width: image.size[0] as u32,
        height: image.size[1] as u32,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Images,
    Containers,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ImageRow {
    repository: String,
    tag: String,
    #[serde(rename = "ID")]
    id: String,
    created_at: String,
    size: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WslcImage {
    repository: String,
    tag: String,
    id: String,
    created: u64,
    size: u64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerRow {
    #[serde(rename = "ID")]
    id: String,
    names: String,
    image: String,
    status: String,
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WslcContainer {
    id: String,
    name: String,
    image: String,
    state: u8,
    state_changed_at: u64,
}

#[derive(Clone)]
struct UpdateInfo {
    version: String,
    notes: String,
    archive_url: String,
    checksum_url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

enum Message {
    WslcInstalled(Result<String, String>),
    UpdateChecked {
        manual: bool,
        result: Result<Option<UpdateInfo>, String>,
    },
    UpdateInstalled(Result<(), String>),
    Images(Result<Vec<ImageRow>, String>),
    Containers(Result<Vec<ContainerRow>, String>),
    ImagesDeleted(Result<String, String>),
    ContainerStarted(Result<String, String>),
    ContainerStopped(Result<String, String>),
    ContainerDeleted(Result<String, String>),
}

struct DevDock {
    page: Page,
    wslc_available: bool,
    wslc_installing: bool,
    wslc_install_error: Option<String>,
    update_checking: bool,
    update_installing: bool,
    update_available: Option<UpdateInfo>,
    update_error: Option<String>,
    images: Vec<ImageRow>,
    containers: Vec<ContainerRow>,
    selected_images: HashSet<String>,
    selected_container: Option<String>,
    image_name_query: String,
    image_id_query: String,
    container_name_query: String,
    container_image_query: String,
    images_loading: bool,
    containers_loading: bool,
    operation_running: bool,
    status: Option<(String, bool)>,
    tx: Sender<Message>,
    rx: Receiver<Message>,
}

impl DevDock {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_chinese_font(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        let wslc_available = is_wslc_available();
        let mut app = Self {
            page: Page::Images,
            wslc_available,
            wslc_installing: false,
            wslc_install_error: None,
            update_checking: false,
            update_installing: false,
            update_available: None,
            update_error: None,
            images: Vec::new(),
            containers: Vec::new(),
            selected_images: HashSet::new(),
            selected_container: None,
            image_name_query: String::new(),
            image_id_query: String::new(),
            container_name_query: String::new(),
            container_image_query: String::new(),
            images_loading: false,
            containers_loading: false,
            operation_running: false,
            status: None,
            tx,
            rx,
        };
        if wslc_available {
            app.refresh_images(cc.egui_ctx.clone());
            app.refresh_containers(cc.egui_ctx.clone());
        }
        app.check_for_updates(false, cc.egui_ctx.clone());
        app
    }

    fn check_for_updates(&mut self, manual: bool, ctx: egui::Context) {
        if self.update_checking || self.update_installing {
            return;
        }
        self.update_checking = true;
        self.update_error = None;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = check_for_update();
            let _ = tx.send(Message::UpdateChecked { manual, result });
            ctx.request_repaint();
        });
    }

    fn install_update(&mut self, ctx: egui::Context) {
        if self.update_installing {
            return;
        }
        let Some(update) = self.update_available.clone() else {
            return;
        };
        self.update_installing = true;
        self.update_error = None;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = install_update(&update);
            let _ = tx.send(Message::UpdateInstalled(result));
            ctx.request_repaint();
        });
    }

    fn install_wslc(&mut self, ctx: egui::Context) {
        if self.wslc_installing {
            return;
        }
        self.wslc_installing = true;
        self.wslc_install_error = None;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = install_wslc();
            let _ = tx.send(Message::WslcInstalled(result));
            ctx.request_repaint();
        });
    }

    fn refresh_images(&mut self, ctx: egui::Context) {
        if self.images_loading {
            return;
        }
        self.images_loading = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = wslc_json::<WslcImage>(&["images", "--no-trunc", "--format", "json"])
                .map(|images| images.into_iter().map(ImageRow::from).collect());
            let _ = tx.send(Message::Images(result));
            ctx.request_repaint();
        });
    }

    fn refresh_containers(&mut self, ctx: egui::Context) {
        if self.containers_loading {
            return;
        }
        self.containers_loading = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result =
                wslc_json::<WslcContainer>(&["ls", "--all", "--no-trunc", "--format", "json"])
                    .map(|containers| containers.into_iter().map(ContainerRow::from).collect());
            let _ = tx.send(Message::Containers(result));
            ctx.request_repaint();
        });
    }

    fn delete_selected_images(&mut self, ctx: egui::Context) {
        if self.operation_running || self.selected_images.is_empty() {
            return;
        }
        self.operation_running = true;
        let targets: Vec<_> = self
            .images
            .iter()
            .filter(|image| self.selected_images.contains(&image.key()))
            .map(ImageRow::delete_target)
            .collect();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = targets
                .iter()
                .try_for_each(|target| run_wslc(&["image", "rm", target]).map(|_| ()))
                .map(|_| String::new());
            let _ = tx.send(Message::ImagesDeleted(result));
            ctx.request_repaint();
        });
    }

    fn stop_container(&mut self, id: String, ctx: egui::Context) {
        if self.operation_running {
            return;
        }
        self.operation_running = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = run_wslc(&["stop", &id]);
            let _ = tx.send(Message::ContainerStopped(result));
            ctx.request_repaint();
        });
    }

    fn start_container(&mut self, id: String, ctx: egui::Context) {
        if self.operation_running {
            return;
        }
        self.operation_running = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = run_wslc(&["start", &id]);
            let _ = tx.send(Message::ContainerStarted(result));
            ctx.request_repaint();
        });
    }

    fn delete_container(&mut self, id: String, ctx: egui::Context) {
        if self.operation_running {
            return;
        }
        self.operation_running = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = run_wslc(&["remove", "--force", &id]);
            let _ = tx.send(Message::ContainerDeleted(result));
            ctx.request_repaint();
        });
    }

    fn receive_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::WslcInstalled(result) => {
                    self.wslc_installing = false;
                    match result {
                        Ok(_) => {
                            self.wslc_available = true;
                            self.status = Some(("WSLC 已安装".to_owned(), false));
                            self.refresh_images(ctx.clone());
                            self.refresh_containers(ctx.clone());
                        }
                        Err(error) => self.wslc_install_error = Some(error),
                    }
                }
                Message::UpdateChecked { manual, result } => {
                    self.update_checking = false;
                    match result {
                        Ok(Some(update)) => self.update_available = Some(update),
                        Ok(None) if manual => {
                            self.status = Some(("当前已是最新版本".to_owned(), false));
                        }
                        Ok(None) => {}
                        Err(error) if manual => self.status = Some((error, true)),
                        Err(_) => {}
                    }
                }
                Message::UpdateInstalled(result) => {
                    self.update_installing = false;
                    match result {
                        Ok(()) => {
                            if let Err(error) = restart_application() {
                                self.update_error = Some(error);
                            }
                        }
                        Err(error) => self.update_error = Some(error),
                    }
                }
                Message::Images(result) => {
                    self.images_loading = false;
                    match result {
                        Ok(images) => {
                            self.images = images;
                            self.selected_images
                                .retain(|key| self.images.iter().any(|image| image.key() == *key));
                        }
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Message::Containers(result) => {
                    self.containers_loading = false;
                    match result {
                        Ok(containers) => {
                            self.containers = containers;
                            if self.selected_container.as_ref().is_some_and(|id| {
                                !self.containers.iter().any(|container| &container.id == id)
                            }) {
                                self.selected_container = None;
                            }
                        }
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Message::ImagesDeleted(result) => {
                    self.operation_running = false;
                    match result {
                        Ok(_) => {
                            self.selected_images.clear();
                            self.status = Some(("所选镜像已删除".to_owned(), false));
                            self.refresh_images(ctx.clone());
                        }
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Message::ContainerStarted(result) => {
                    self.operation_running = false;
                    match result {
                        Ok(_) => {
                            self.status = Some(("容器已启动".to_owned(), false));
                            self.refresh_containers(ctx.clone());
                        }
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Message::ContainerStopped(result) => {
                    self.operation_running = false;
                    match result {
                        Ok(_) => {
                            self.status = Some(("容器已停止".to_owned(), false));
                            self.refresh_containers(ctx.clone());
                        }
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Message::ContainerDeleted(result) => {
                    self.operation_running = false;
                    match result {
                        Ok(_) => {
                            self.status = Some(("容器已删除".to_owned(), false));
                            self.refresh_containers(ctx.clone());
                        }
                        Err(error) => self.status = Some((error, true)),
                    }
                }
            }
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("navigation")
            .resizable(false)
            .exact_size(150.0)
            .show(ui, |ui| {
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::from_bytes(
                            "bytes://devdock-logo.svg",
                            include_bytes!("../assets/logo.svg"),
                        )
                        .fit_to_exact_size(egui::vec2(34.0, 34.0)),
                    );
                    ui.heading(RichText::new("DevDock").size(21.0));
                });
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("WSLC管理工具").weak());
                });
                ui.add_space(28.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::selectable(
                            self.page == Page::Images,
                            RichText::new("镜像列表").size(15.0),
                        ),
                    )
                    .clicked()
                {
                    self.page = Page::Images;
                }
                ui.add_space(6.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::selectable(
                            self.page == Page::Containers,
                            RichText::new("容器列表").size(15.0),
                        ),
                    )
                    .clicked()
                {
                    self.page = Page::Containers;
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("v{APP_VERSION}")).small().weak());
                    ui.add_space(4.0);
                    if ui
                        .add_enabled(
                            !self.update_checking && !self.update_installing,
                            egui::Button::new(if self.update_checking {
                                "检查中..."
                            } else {
                                "检查更新"
                            }),
                        )
                        .clicked()
                    {
                        self.check_for_updates(true, ui.ctx().clone());
                    }
                });
            });
    }

    fn wslc_install_modal(&mut self, ctx: &egui::Context) {
        if self.wslc_available {
            return;
        }
        egui::Modal::new("wslc_install_required".into()).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("需要安装 WSLC");
            ui.add_space(8.0);
            ui.label("DevDock 依赖 WSLC 管理镜像和容器。安装完成前无法使用主界面。");
            ui.add_space(12.0);

            if self.wslc_installing {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在安装或更新 WSL，请按系统提示完成操作...");
                });
            } else {
                if let Some(error) = &self.wslc_install_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    ui.add_space(8.0);
                }
                let button_text = if self.wslc_install_error.is_some() {
                    "重试安装"
                } else {
                    "安装 WSLC"
                };
                if ui.button(button_text).clicked() {
                    self.install_wslc(ctx.clone());
                }
            }
        });
    }

    fn update_modal(&mut self, ctx: &egui::Context) {
        if !self.wslc_available {
            return;
        }
        let Some(update) = self.update_available.clone() else {
            return;
        };
        egui::Modal::new("application_update".into()).show(ctx, |ui| {
            ui.set_width(430.0);
            ui.heading(if self.update_installing {
                "正在更新 DevDock"
            } else {
                "发现新版本"
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(format!("当前版本：v{APP_VERSION}"));
                ui.separator();
                ui.strong(format!("最新版本：v{}", update.version));
            });

            if !update.notes.trim().is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new("更新说明").strong());
                egui::ScrollArea::vertical()
                    .id_salt("update_notes")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        ui.label(&update.notes);
                    });
            }

            ui.add_space(12.0);
            if self.update_installing {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在下载、校验并安装更新，请勿关闭程序...");
                });
            } else {
                if let Some(error) = &self.update_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    ui.add_space(8.0);
                }
                ui.horizontal(|ui| {
                    if ui.button("稍后").clicked() {
                        self.update_available = None;
                        self.update_error = None;
                    }
                    if ui
                        .add_enabled(
                            !self.operation_running,
                            egui::Button::new(if self.update_error.is_some() {
                                "重试更新"
                            } else {
                                "立即更新"
                            }),
                        )
                        .clicked()
                    {
                        self.install_update(ctx.clone());
                    }
                });
            }
        });
    }

    fn image_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("镜像列表");
            ui.add_space(12.0);
            if ui
                .add_enabled(!self.images_loading, egui::Button::new("刷新"))
                .clicked()
            {
                self.refresh_images(ui.ctx().clone());
            }
            if self.images_loading {
                ui.spinner();
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.image_name_query)
                    .hint_text("镜像名称")
                    .desired_width(190.0),
            );
            ui.add_space(10.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.image_id_query)
                    .hint_text("镜像 ID")
                    .desired_width(190.0),
            );
            ui.add_space(10.0);
            let delete_text = format!("删除 ({})", self.selected_images.len());
            if ui
                .add_enabled(
                    !self.selected_images.is_empty() && !self.operation_running,
                    egui::Button::new(delete_text),
                )
                .clicked()
            {
                self.delete_selected_images(ui.ctx().clone());
            }
            if self.operation_running {
                ui.spinner();
            }
        });
        ui.add_space(12.0);

        let name_query = self.image_name_query.trim().to_lowercase();
        let id_query = self.image_id_query.trim().to_lowercase();
        let images: Vec<_> = self
            .images
            .iter()
            .filter(|image| {
                image.repository.to_lowercase().contains(&name_query)
                    && image.id.to_lowercase().contains(&id_query)
            })
            .cloned()
            .collect();

        style_table_rows(ui);
        egui::ScrollArea::horizontal()
            .id_salt("images_horizontal_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(1_250.0);
                let table = TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(34.0))
                    .column(Column::initial(300.0).at_least(170.0))
                    .column(Column::initial(140.0).at_least(70.0))
                    .column(Column::initial(100.0).at_least(70.0))
                    .column(Column::initial(180.0).at_least(130.0))
                    .column(Column::remainder().at_least(450.0))
                    .header(32.0, |mut header| {
                        header.col(|_| {});
                        header.col(|ui| {
                            ui.strong("镜像名称");
                        });
                        header.col(|ui| {
                            ui.strong("标签");
                        });
                        header.col(|ui| {
                            ui.strong("大小");
                        });
                        header.col(|ui| {
                            ui.strong("创建时间");
                        });
                        header.col(|ui| {
                            ui.strong("镜像 ID");
                        });
                    });

                table.body(|mut body| {
                    for image in images {
                        body.row(30.0, |mut row| {
                            let key = image.key();
                            row.set_selected(self.selected_images.contains(&key));
                            let mut checkbox_changed = false;
                            row.col(|ui| {
                                let mut selected = self.selected_images.contains(&key);
                                if ui.checkbox(&mut selected, "").changed() {
                                    checkbox_changed = true;
                                    if selected {
                                        self.selected_images.insert(key.clone());
                                    } else {
                                        self.selected_images.remove(&key);
                                    }
                                }
                            });
                            row.col(|ui| {
                                ui.label(&image.repository);
                            });
                            row.col(|ui| {
                                ui.label(&image.tag);
                            });
                            row.col(|ui| {
                                ui.label(&image.size);
                            });
                            row.col(|ui| {
                                ui.label(&image.created_at);
                            });
                            row.col(|ui| {
                                ui.label(&image.id);
                            });
                            let response = row.response();
                            if response.clicked() && !checkbox_changed {
                                if self.selected_images.contains(&key) {
                                    self.selected_images.remove(&key);
                                } else {
                                    self.selected_images.insert(key.clone());
                                }
                            }
                            if response.secondary_clicked() {
                                self.selected_images.insert(key);
                            }
                            image_context_menu(&response, &image);
                        });
                    }
                });
            });
    }

    fn container_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("容器列表");
            ui.add_space(12.0);
            if ui
                .add_enabled(!self.containers_loading, egui::Button::new("刷新"))
                .clicked()
            {
                self.refresh_containers(ui.ctx().clone());
            }
            if self.containers_loading {
                ui.spinner();
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.container_name_query)
                    .hint_text("容器名称")
                    .desired_width(190.0),
            );
            ui.add_space(10.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.container_image_query)
                    .hint_text("镜像名称")
                    .desired_width(190.0),
            );
            if self.operation_running {
                ui.spinner();
            }
        });
        ui.add_space(12.0);

        let name_query = self.container_name_query.trim().to_lowercase();
        let image_query = self.container_image_query.trim().to_lowercase();
        let containers: Vec<_> = self
            .containers
            .iter()
            .filter(|container| {
                container.names.to_lowercase().contains(&name_query)
                    && container.image.to_lowercase().contains(&image_query)
            })
            .cloned()
            .collect();

        style_table_rows(ui);
        egui::ScrollArea::horizontal()
            .id_salt("containers_horizontal_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(1_250.0);
                TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(125.0))
                    .column(Column::initial(180.0).at_least(140.0))
                    .column(Column::initial(150.0).at_least(100.0))
                    .column(Column::initial(340.0).at_least(160.0))
                    .column(Column::remainder().at_least(450.0))
                    .header(32.0, |mut header| {
                        header.col(|ui| {
                            ui.strong("操作");
                        });
                        header.col(|ui| {
                            ui.strong("容器名称");
                        });
                        header.col(|ui| {
                            ui.strong("状态");
                        });
                        header.col(|ui| {
                            ui.strong("镜像名称");
                        });
                        header.col(|ui| {
                            ui.strong("容器 ID");
                        });
                    })
                    .body(|mut body| {
                        for container in containers {
                            body.row(30.0, |mut row| {
                                row.set_selected(
                                    self.selected_container.as_ref() == Some(&container.id),
                                );
                                row.col(|ui| {
                                    ui.horizontal(|ui| {
                                        let running =
                                            container.state.eq_ignore_ascii_case("running");
                                        if ui
                                            .add_enabled(
                                                !self.operation_running,
                                                egui::Button::new(if running {
                                                    "停止"
                                                } else {
                                                    "启动"
                                                }),
                                            )
                                            .clicked()
                                        {
                                            if running {
                                                self.stop_container(
                                                    container.id.clone(),
                                                    ui.ctx().clone(),
                                                );
                                            } else {
                                                self.start_container(
                                                    container.id.clone(),
                                                    ui.ctx().clone(),
                                                );
                                            }
                                        }
                                        if ui
                                            .add_enabled(
                                                !self.operation_running,
                                                egui::Button::new("删除"),
                                            )
                                            .clicked()
                                        {
                                            self.delete_container(
                                                container.id.clone(),
                                                ui.ctx().clone(),
                                            );
                                        }
                                    });
                                });
                                row.col(|ui| {
                                    ui.label(&container.names);
                                });
                                row.col(|ui| {
                                    let running = container.state.eq_ignore_ascii_case("running");
                                    let color = if running {
                                        Color32::from_rgb(43, 166, 102)
                                    } else {
                                        ui.visuals().weak_text_color()
                                    };
                                    ui.label(RichText::new(&container.status).color(color));
                                });
                                row.col(|ui| {
                                    ui.label(&container.image);
                                });
                                row.col(|ui| {
                                    ui.label(&container.id);
                                });
                                let response = row.response();
                                if response.clicked() || response.secondary_clicked() {
                                    self.selected_container = Some(container.id.clone());
                                }
                                container_context_menu(&response, &container);
                            });
                        }
                    });
            });
    }
}

impl ImageRow {
    fn key(&self) -> String {
        format!("{}\0{}\0{}", self.repository, self.tag, self.id)
    }

    fn delete_target(&self) -> String {
        if self.repository == "<none>" || self.tag == "<none>" {
            self.id.clone()
        } else {
            format!("{}:{}", self.repository, self.tag)
        }
    }
}

impl From<WslcImage> for ImageRow {
    fn from(image: WslcImage) -> Self {
        Self {
            repository: image.repository,
            tag: image.tag,
            id: image.id,
            created_at: relative_time(image.created),
            size: human_size(image.size),
        }
    }
}

impl From<WslcContainer> for ContainerRow {
    fn from(container: WslcContainer) -> Self {
        let state = match container.state {
            1 => "created",
            2 => "running",
            3 => "exited",
            4 => "paused",
            _ => "unknown",
        };
        Self {
            id: container.id,
            names: container.name,
            image: container.image,
            status: format!("{} {}", state, relative_time(container.state_changed_at)),
            state: state.to_owned(),
        }
    }
}

impl eframe::App for DevDock {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive_messages(ui.ctx());
        self.sidebar(ui);
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some((message, is_error)) = &self.status {
                    let color = if *is_error {
                        ui.visuals().error_fg_color
                    } else {
                        Color32::from_rgb(43, 166, 102)
                    };
                    ui.label(RichText::new(message).color(color));
                } else {
                    ui.label(RichText::new("就绪").weak());
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(14.0);
            match self.page {
                Page::Images => self.image_page(ui),
                Page::Containers => self.container_page(ui),
            }
        });
        self.update_modal(ui.ctx());
        self.wslc_install_modal(ui.ctx());
    }
}

fn style_table_rows(ui: &mut egui::Ui) {
    ui.style_mut().interaction.selectable_labels = false;
    let dark_mode = ui.visuals().dark_mode;
    let visuals = ui.visuals_mut();
    visuals.widgets.hovered.bg_fill = if dark_mode {
        Color32::from_rgba_unmultiplied(90, 150, 225, 28)
    } else {
        Color32::from_rgba_unmultiplied(45, 105, 185, 18)
    };
    visuals.selection.bg_fill = if dark_mode {
        Color32::from_rgba_unmultiplied(75, 135, 210, 52)
    } else {
        Color32::from_rgba_unmultiplied(45, 105, 185, 34)
    };
    visuals.selection.stroke.color = if dark_mode {
        Color32::from_rgb(190, 215, 245)
    } else {
        Color32::from_rgb(35, 80, 140)
    };
}

fn image_context_menu(response: &egui::Response, image: &ImageRow) {
    response.context_menu(|ui| {
        if ui.button("复制镜像名称").clicked() {
            ui.ctx().copy_text(image.repository.clone());
            ui.close();
        }
        if ui.button("复制镜像 ID").clicked() {
            ui.ctx().copy_text(image.id.clone());
            ui.close();
        }
    });
}

fn container_context_menu(response: &egui::Response, container: &ContainerRow) {
    response.context_menu(|ui| {
        if ui.button("复制容器名称").clicked() {
            ui.ctx().copy_text(container.names.clone());
            ui.close();
        }
        if ui.button("复制容器 ID").clicked() {
            ui.ctx().copy_text(container.id.clone());
            ui.close();
        }
    });
}

fn check_for_update() -> Result<Option<UpdateInfo>, String> {
    match http_get_text(GITHUB_LATEST_RELEASE) {
        Ok(response) => {
            let release: GithubRelease = serde_json::from_str(&response)
                .map_err(|error| format!("无法解析 GitHub Release：{error}"))?;
            update_from_release(release)
        }
        Err(api_error) => check_for_update_from_redirect()
            .map_err(|error| format!("{api_error}；备用更新检查失败：{error}")),
    }
}

fn check_for_update_from_redirect() -> Result<Option<UpdateInfo>, String> {
    let response = ureq::head(GITHUB_LATEST_PAGE)
        .header("User-Agent", "DevDock-Updater")
        .call()
        .map_err(|error| format!("网络请求失败：{error}"))?;
    let tag = response
        .get_uri()
        .path()
        .rsplit('/')
        .next()
        .filter(|tag| !tag.is_empty() && *tag != "latest")
        .ok_or_else(|| "GitHub 未返回最新版本标签".to_owned())?;
    update_from_tag(tag)
}

fn update_from_tag(tag: &str) -> Result<Option<UpdateInfo>, String> {
    let version_text = tag.trim_start_matches('v');
    let latest = Version::parse(version_text)
        .map_err(|error| format!("Release 版本号无效（{tag}）：{error}"))?;
    let current = Version::parse(APP_VERSION)
        .map_err(|error| format!("当前版本号无效（{APP_VERSION}）：{error}"))?;
    if latest <= current {
        return Ok(None);
    }
    let archive_name = format!("DevDock-{latest}-windows-x64.zip");
    let download_base = format!("https://github.com/mickcui/DevDock/releases/download/{tag}");
    Ok(Some(UpdateInfo {
        version: latest.to_string(),
        notes: format!("请查看发布说明：https://github.com/mickcui/DevDock/releases/tag/{tag}"),
        archive_url: format!("{download_base}/{archive_name}"),
        checksum_url: format!("{download_base}/{archive_name}.sha256"),
    }))
}

fn update_from_release(release: GithubRelease) -> Result<Option<UpdateInfo>, String> {
    let version_text = release.tag_name.trim_start_matches('v');
    let latest = Version::parse(version_text)
        .map_err(|error| format!("Release 版本号无效（{}）：{error}", release.tag_name))?;
    let current = Version::parse(APP_VERSION)
        .map_err(|error| format!("当前版本号无效（{APP_VERSION}）：{error}"))?;
    if latest <= current {
        return Ok(None);
    }

    let archive_name = format!("DevDock-{latest}-windows-x64.zip");
    let checksum_name = format!("{archive_name}.sha256");
    let archive_url = release
        .assets
        .iter()
        .find(|asset| asset.name == archive_name)
        .map(|asset| asset.browser_download_url.clone())
        .ok_or_else(|| format!("Release 中缺少更新包：{archive_name}"))?;
    let checksum_url = release
        .assets
        .iter()
        .find(|asset| asset.name == checksum_name)
        .map(|asset| asset.browser_download_url.clone())
        .ok_or_else(|| format!("Release 中缺少校验文件：{checksum_name}"))?;

    Ok(Some(UpdateInfo {
        version: latest.to_string(),
        notes: release
            .body
            .or(release.name)
            .unwrap_or_else(|| "此版本未提供更新说明。".to_owned()),
        archive_url,
        checksum_url,
    }))
}

fn install_update(update: &UpdateInfo) -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|error| format!("无法确定当前程序路径：{error}"))?;
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| "无法确定程序所在目录".to_owned())?;
    let temp_dir = tempfile::Builder::new()
        .prefix(".devdock-update-")
        .tempdir_in(install_dir)
        .map_err(|error| format!("无法在程序目录创建更新文件：{error}"))?;
    let archive_path = temp_dir.path().join("update.zip");

    let expected_checksum = parse_expected_checksum(&http_get_text(&update.checksum_url)?)?;
    download_to_file(&update.archive_url, &archive_path)?;
    verify_file_checksum(&archive_path, &expected_checksum)?;

    let archive_file =
        File::open(&archive_path).map_err(|error| format!("无法打开更新包：{error}"))?;
    let mut archive =
        zip::ZipArchive::new(archive_file).map_err(|error| format!("更新包格式无效：{error}"))?;
    let mut executable = archive
        .by_name("DevDock.exe")
        .map_err(|error| format!("更新包中缺少 DevDock.exe：{error}"))?;
    let new_exe = temp_dir.path().join("DevDock.exe");
    let mut output =
        File::create(&new_exe).map_err(|error| format!("无法创建新版程序：{error}"))?;
    std::io::copy(&mut executable, &mut output)
        .map_err(|error| format!("无法解压新版程序：{error}"))?;
    output
        .flush()
        .map_err(|error| format!("无法写入新版程序：{error}"))?;
    drop(output);
    drop(executable);
    drop(archive);

    self_replace::self_replace(&new_exe).map_err(|error| format!("无法替换当前程序：{error}"))
}

fn http_get_text(url: &str) -> Result<String, String> {
    let mut response = ureq::get(url)
        .header("User-Agent", "DevDock-Updater")
        .call()
        .map_err(|error| format!("网络请求失败：{error}"))?;
    response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("读取网络响应失败：{error}"))
}

fn download_to_file(url: &str, path: &std::path::Path) -> Result<(), String> {
    let response = ureq::get(url)
        .header("User-Agent", "DevDock-Updater")
        .call()
        .map_err(|error| format!("下载更新包失败：{error}"))?;
    let mut reader = response.into_body().into_reader();
    let mut file = File::create(path).map_err(|error| format!("无法保存更新包：{error}"))?;
    std::io::copy(&mut reader, &mut file).map_err(|error| format!("下载更新包失败：{error}"))?;
    file.flush()
        .map_err(|error| format!("无法保存更新包：{error}"))
}

fn parse_expected_checksum(content: &str) -> Result<String, String> {
    let checksum = content
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Release 中的 SHA256 校验值无效".to_owned());
    }
    Ok(checksum)
}

fn verify_file_checksum(path: &std::path::Path, expected: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| format!("无法读取更新包：{error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("无法校验更新包：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual == expected {
        Ok(())
    } else {
        Err("更新包 SHA256 校验失败，已取消安装".to_owned())
    }
}

fn restart_application() -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("更新完成，但无法确定程序路径：{error}"))?;
    Command::new(executable)
        .spawn()
        .map_err(|error| format!("更新完成，但无法重新启动程序：{error}"))?;
    std::process::exit(0);
}

fn wslc_json<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<Vec<T>, String> {
    let output = run_wslc(args)?;
    serde_json::from_str(&output).map_err(|error| format!("无法解析 WSLC 返回数据：{error}"))
}

fn run_wslc(args: &[&str]) -> Result<String, String> {
    let output = Command::new(wslc_executable())
        .args(args)
        .output()
        .map_err(|error| format!("无法运行 wslc，请确认 WSLC 已安装：{error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if error.is_empty() {
            format!("WSLC 命令执行失败（{}）", output.status)
        } else {
            error
        })
    }
}

fn wslc_executable() -> std::path::PathBuf {
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        let path = std::path::PathBuf::from(program_files)
            .join("WSL")
            .join("wslc.exe");
        if path.is_file() {
            return path;
        }
    }
    "wslc".into()
}

fn is_wslc_available() -> bool {
    Command::new(wslc_executable())
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn install_wslc() -> Result<String, String> {
    let wsl_available = Command::new("wsl")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !wsl_available {
        run_install_command(
            "winget",
            &[
                "install",
                "--id",
                "Microsoft.WSL",
                "--exact",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--silent",
            ],
            "安装 WSL 失败",
        )?;
    }

    run_install_command("wsl", &["--update", "--pre-release"], "更新 WSL 失败")?;
    if is_wslc_available() {
        Ok("WSLC 已安装".to_owned())
    } else {
        Err("安装已完成，但尚未检测到 WSLC。请重启 Windows 后重试。".to_owned())
    }
}

fn run_install_command(
    executable: &str,
    args: &[&str],
    failure_message: &str,
) -> Result<String, String> {
    let output = Command::new(executable)
        .args(args)
        .output()
        .map_err(|error| format!("{failure_message}：无法运行 {executable}：{error}"))?;
    let stdout = decode_command_output(&output.stdout);
    let stderr = decode_command_output(&output.stderr);
    if output.status.success() {
        Ok(stdout.trim().to_owned())
    } else {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        Err(if detail.is_empty() {
            format!("{failure_message}（{}）", output.status)
        } else {
            format!("{failure_message}：{detail}")
        })
    }
}

fn decode_command_output(bytes: &[u8]) -> String {
    if bytes.len() >= 2
        && bytes.iter().skip(1).step_by(2).filter(|&&b| b == 0).count() > bytes.len() / 4
    {
        let utf16: Vec<_> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&utf16)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn relative_time(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(timestamp, |duration| duration.as_secs());
    let seconds = now.saturating_sub(timestamp);
    if seconds < 60 {
        format!("{seconds} 秒前")
    } else if seconds < 3_600 {
        format!("{} 分钟前", seconds / 60)
    } else if seconds < 86_400 {
        format!("{} 小时前", seconds / 3_600)
    } else if seconds < 2_592_000 {
        format!("{} 天前", seconds / 86_400)
    } else if seconds < 31_536_000 {
        format!("{} 个月前", seconds / 2_592_000)
    } else {
        format!("{} 年前", seconds / 31_536_000)
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1_000.0 && unit < UNITS.len() - 1 {
        size /= 1_000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{size:.2} {}", UNITS[unit])
    }
}

fn install_chinese_font(ctx: &egui::Context) {
    let paths = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
    ];
    let Some(data) = paths.iter().find_map(|path| std::fs::read(path).ok()) else {
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "windows_chinese".to_owned(),
        FontData::from_owned(data).into(),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "windows_chinese".to_owned());
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_assets() {
        let release: GithubRelease = serde_json::from_str(
            r#"{
                "tag_name": "v999.0.0",
                "name": "Release v999.0.0",
                "body": "Update notes",
                "assets": [
                    {
                        "name": "DevDock-999.0.0-windows-x64.zip",
                        "browser_download_url": "https://example.com/update.zip"
                    },
                    {
                        "name": "DevDock-999.0.0-windows-x64.zip.sha256",
                        "browser_download_url": "https://example.com/update.zip.sha256"
                    }
                ]
            }"#,
        )
        .unwrap();

        let update = update_from_release(release).unwrap().unwrap();
        assert_eq!(update.version, "999.0.0");
        assert_eq!(update.notes, "Update notes");
        assert_eq!(update.archive_url, "https://example.com/update.zip");
        assert_eq!(update.checksum_url, "https://example.com/update.zip.sha256");
    }

    #[test]
    fn builds_fallback_release_urls() {
        let update = update_from_tag("v999.0.0").unwrap().unwrap();
        assert_eq!(update.version, "999.0.0");
        assert_eq!(
            update.archive_url,
            "https://github.com/mickcui/DevDock/releases/download/v999.0.0/DevDock-999.0.0-windows-x64.zip"
        );
        assert_eq!(
            update.checksum_url,
            "https://github.com/mickcui/DevDock/releases/download/v999.0.0/DevDock-999.0.0-windows-x64.zip.sha256"
        );
    }

    #[test]
    fn parses_sha256_file() {
        let hash = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789";
        assert_eq!(
            parse_expected_checksum(&format!("{hash}  DevDock.zip")).unwrap(),
            hash.to_ascii_lowercase()
        );
        assert!(parse_expected_checksum("invalid").is_err());
    }

    #[test]
    fn verifies_download_checksum() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"abc").unwrap();
        assert!(
            verify_file_checksum(
                file.path(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            )
            .is_ok()
        );
        assert!(verify_file_checksum(file.path(), &"0".repeat(64)).is_err());
    }
}
