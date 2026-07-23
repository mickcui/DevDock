#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod terminal;

#[cfg(not(target_os = "windows"))]
compile_error!("DevDock only supports Windows");

use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
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

use terminal::TerminalSession;

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
        centered: true,
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
    Shell,
    Settings,
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
    ports: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WslcPort {
    binding_address: String,
    container_port: u16,
    host_port: u16,
    protocol: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WslcContainer {
    id: String,
    name: String,
    image: String,
    state: u8,
    state_changed_at: u64,
    #[serde(default)]
    ports: Vec<WslcPort>,
}

#[derive(Clone, Default)]
struct CreateContainerForm {
    image: String,
    command: String,
    arguments: String,
    cidfile: String,
    cpus: String,
    detach: bool,
    dns: String,
    dns_option: String,
    dns_search: String,
    domainname: String,
    entrypoint: String,
    env: String,
    env_file: String,
    gpus: String,
    hostname: String,
    interactive: bool,
    label: String,
    memory: String,
    name: String,
    network: String,
    network_alias: String,
    publish: String,
    publish_all: bool,
    remove: bool,
    shm_size: String,
    stop_signal: String,
    tmpfs: String,
    tty: bool,
    ulimit: String,
    user: String,
    volume: String,
    workdir: String,
}

impl CreateContainerForm {
    fn to_args(&self) -> Result<Vec<String>, String> {
        let image = self.image.trim();
        if image.is_empty() {
            return Err("请输入镜像名称".to_owned());
        }
        if !self.cpus.trim().is_empty()
            && self
                .cpus
                .trim()
                .parse::<f64>()
                .map_or(true, |cpus| !cpus.is_finite() || cpus <= 0.0)
        {
            return Err("CPU 数必须是大于 0 的数字".to_owned());
        }

        let mut args = vec!["run".to_owned()];
        push_option(&mut args, "--cidfile", &self.cidfile);
        push_option(&mut args, "--cpus", &self.cpus);
        push_flag(&mut args, "--detach", self.detach);
        push_repeated_options(&mut args, "--dns", &self.dns);
        push_repeated_options(&mut args, "--dns-option", &self.dns_option);
        push_repeated_options(&mut args, "--dns-search", &self.dns_search);
        push_option(&mut args, "--domainname", &self.domainname);
        push_option(&mut args, "--entrypoint", &self.entrypoint);
        push_repeated_options(&mut args, "--env", &self.env);
        push_repeated_options(&mut args, "--env-file", &self.env_file);
        push_option(&mut args, "--gpus", &self.gpus);
        push_option(&mut args, "--hostname", &self.hostname);
        push_flag(&mut args, "--interactive", self.interactive);
        push_repeated_options(&mut args, "--label", &self.label);
        push_option(&mut args, "--memory", &self.memory);
        push_option(&mut args, "--name", &self.name);
        push_option(&mut args, "--network", &self.network);
        push_repeated_options(&mut args, "--network-alias", &self.network_alias);
        push_repeated_options(&mut args, "--publish", &self.publish);
        push_flag(&mut args, "--publish-all", self.publish_all);
        push_flag(&mut args, "--rm", self.remove);
        push_option(&mut args, "--shm-size", &self.shm_size);
        push_option(&mut args, "--stop-signal", &self.stop_signal);
        push_repeated_options(&mut args, "--tmpfs", &self.tmpfs);
        push_flag(&mut args, "--tty", self.tty);
        push_repeated_options(&mut args, "--ulimit", &self.ulimit);
        push_option(&mut args, "--user", &self.user);
        push_repeated_options(&mut args, "--volume", &self.volume);
        push_option(&mut args, "--workdir", &self.workdir);
        args.push(image.to_owned());
        if !self.command.trim().is_empty() {
            args.push(self.command.trim().to_owned());
        }
        args.extend(non_empty_lines(&self.arguments));
        Ok(args)
    }
}

fn push_option(args: &mut Vec<String>, option: &str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        args.extend([option.to_owned(), value.to_owned()]);
    }
}

fn push_flag(args: &mut Vec<String>, option: &str, enabled: bool) {
    if enabled {
        args.push(option.to_owned());
    }
}

fn push_repeated_options(args: &mut Vec<String>, option: &str, values: &str) {
    for value in non_empty_lines(values) {
        args.extend([option.to_owned(), value]);
    }
}

fn non_empty_lines(values: &str) -> impl Iterator<Item = String> + '_ {
    values
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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
    ImagePullOutput(String),
    ImagePulled(Result<(), String>),
    ContainerStarted(Result<String, String>),
    ContainerStopped(Result<String, String>),
    ContainerDeleted(Result<String, String>),
    ContainerCreated(Result<String, String>),
}

struct DevDock {
    page: Page,
    wslc_available: bool,
    wslc_version: Option<String>,
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
    image_pull_name: String,
    image_pull_log: String,
    image_pull_open: bool,
    image_pulling: bool,
    image_delete_confirm_open: bool,
    container_delete_confirm_open: bool,
    container_create_open: bool,
    container_create_form: CreateContainerForm,
    terminal: Option<TerminalSession>,
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
        let wslc_version = get_wslc_version();
        let wslc_available = wslc_version.is_some();
        let mut app = Self {
            page: Page::Images,
            wslc_available,
            wslc_version,
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
            image_pull_name: String::new(),
            image_pull_log: String::new(),
            image_pull_open: false,
            image_pulling: false,
            image_delete_confirm_open: false,
            container_delete_confirm_open: false,
            container_create_open: false,
            container_create_form: CreateContainerForm::default(),
            terminal: None,
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
            .map(ImageRow::command_target)
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

    fn pull_image(&mut self, ctx: egui::Context) {
        let image = self.image_pull_name.trim().to_owned();
        if self.operation_running || image.is_empty() {
            return;
        }
        self.operation_running = true;
        self.image_pulling = true;
        self.image_pull_log = format!("$ wslc pull {image}\n\n");
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = run_wslc_pull(&image, tx.clone(), ctx.clone());
            let _ = tx.send(Message::ImagePulled(result));
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

    fn create_container(&mut self, ctx: egui::Context) {
        if self.operation_running {
            return;
        }
        let args = match self.container_create_form.to_args() {
            Ok(args) => args,
            Err(error) => {
                self.status = Some((error, true));
                return;
            }
        };
        self.operation_running = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = run_wslc_args(&args);
            let _ = tx.send(Message::ContainerCreated(result));
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
                            self.wslc_version = get_wslc_version();
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
                            self.image_delete_confirm_open = false;
                            self.selected_images.clear();
                            self.status = Some(("所选镜像已删除".to_owned(), false));
                            self.refresh_images(ctx.clone());
                        }
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Message::ImagePullOutput(output) => {
                    self.image_pull_log.push_str(&output);
                }
                Message::ImagePulled(result) => {
                    self.operation_running = false;
                    self.image_pulling = false;
                    match result {
                        Ok(()) => {
                            self.image_pull_log.push_str("\n镜像拉取完成。\n");
                            self.status = Some(("镜像拉取完成".to_owned(), false));
                            self.refresh_images(ctx.clone());
                        }
                        Err(error) => {
                            self.image_pull_log.push_str(&format!("\n{error}\n"));
                            self.status = Some((error, true));
                        }
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
                    self.container_delete_confirm_open = false;
                    match result {
                        Ok(_) => {
                            self.status = Some(("容器已删除".to_owned(), false));
                            self.refresh_containers(ctx.clone());
                        }
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Message::ContainerCreated(result) => {
                    self.operation_running = false;
                    match result {
                        Ok(_) => {
                            self.container_create_open = false;
                            self.status = Some(("容器已创建".to_owned(), false));
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
                if let Some(terminal) = &self.terminal {
                    ui.add_space(6.0);
                    let title = format!("Shell · {}", terminal.container_name);
                    if ui
                        .add_sized(
                            [ui.available_width(), 34.0],
                            egui::Button::selectable(
                                self.page == Page::Shell,
                                RichText::new(title).size(14.0),
                            ),
                        )
                        .clicked()
                    {
                        self.page = Page::Shell;
                    }
                }
                ui.add_space(6.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::selectable(
                            self.page == Page::Settings,
                            RichText::new("设置").size(15.0),
                        ),
                    )
                    .clicked()
                {
                    self.page = Page::Settings;
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("DevDock v{APP_VERSION}"))
                            .size(13.0)
                            .weak(),
                    );
                    ui.label(
                        RichText::new(match &self.wslc_version {
                            Some(version) => format!("WSLC v{version}"),
                            None => "WSLC 未安装".to_owned(),
                        })
                        .size(13.0)
                        .weak(),
                    );
                    ui.add_space(6.0);
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
        egui::Frame::group(ui.style())
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let search_width = ((ui.available_width() - 300.0) / 2.0).clamp(145.0, 190.0);
                    ui.add_sized(
                        [search_width, 34.0],
                        egui::TextEdit::singleline(&mut self.image_name_query)
                            .hint_text("镜像名称")
                            .font(egui::FontId::proportional(14.0))
                            .vertical_align(egui::Align::Center),
                    );
                    ui.add_sized(
                        [search_width, 34.0],
                        egui::TextEdit::singleline(&mut self.image_id_query)
                            .hint_text("镜像 ID")
                            .font(egui::FontId::proportional(14.0))
                            .vertical_align(egui::Align::Center),
                    );
                    ui.separator();

                    let delete_text = format!("删除 ({})", self.selected_images.len());
                    let delete_color = ui.visuals().error_fg_color;
                    if ui
                        .add_enabled(
                            !self.selected_images.is_empty() && !self.operation_running,
                            egui::Button::new(RichText::new(delete_text).color(delete_color))
                                .stroke(egui::Stroke::new(1.0, delete_color))
                                .min_size(egui::vec2(90.0, 34.0)),
                        )
                        .clicked()
                    {
                        self.image_delete_confirm_open = true;
                    }
                    if ui
                        .add_enabled(
                            !self.operation_running,
                            egui::Button::new(RichText::new("拉取").color(Color32::WHITE))
                                .fill(Color32::from_rgb(50, 105, 190))
                                .min_size(egui::vec2(76.0, 34.0)),
                        )
                        .clicked()
                    {
                        self.image_pull_name.clear();
                        self.image_pull_log.clear();
                        self.image_pull_open = true;
                    }
                    if self.operation_running {
                        ui.spinner();
                    }
                });
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
        if images.is_empty() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(120.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(30.0);
                    let message = if self.images_loading {
                        "正在加载镜像..."
                    } else if self.images.is_empty() {
                        "暂无本地镜像"
                    } else {
                        "没有符合筛选条件的镜像"
                    };
                    ui.label(RichText::new(message).size(15.0).weak());
                    if !self.images_loading && self.images.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new("点击右上角“拉取”添加镜像").small().weak());
                    }
                });
            });
            return;
        }

        let visible_image_keys: HashSet<_> = images.iter().map(ImageRow::key).collect();
        egui::Frame::group(ui.style())
            .inner_margin(0.0)
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("images_horizontal_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(960.0);
                        let header_color = ui.visuals().weak_text_color();
                        let table = TableBuilder::new(ui)
                            .striped(true)
                            .resizable(true)
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::exact(36.0))
                            .column(Column::initial(280.0).at_least(180.0))
                            .column(Column::initial(110.0).at_least(80.0))
                            .column(Column::initial(100.0).at_least(80.0))
                            .column(Column::initial(150.0).at_least(120.0))
                            .column(Column::remainder().at_least(300.0))
                            .header(38.0, |mut header| {
                                header.col(|ui| {
                                    ui.centered_and_justified(|ui| {
                                        let mut all_selected = visible_image_keys
                                            .iter()
                                            .all(|key| self.selected_images.contains(key));
                                        if ui
                                            .checkbox(&mut all_selected, "")
                                            .on_hover_text("全选当前筛选结果")
                                            .changed()
                                        {
                                            if all_selected {
                                                self.selected_images
                                                    .extend(visible_image_keys.iter().cloned());
                                            } else {
                                                self.selected_images.retain(|key| {
                                                    !visible_image_keys.contains(key)
                                                });
                                            }
                                        }
                                    });
                                });
                                for title in ["镜像名称", "标签", "大小", "创建时间", "镜像 ID"]
                                {
                                    header.col(|ui| {
                                        ui.label(RichText::new(title).strong().color(header_color));
                                    });
                                }
                            });

                        table.body(|mut body| {
                            for image in images {
                                body.row(40.0, |mut row| {
                                    let key = image.key();
                                    row.set_selected(self.selected_images.contains(&key));
                                    let mut checkbox_changed = false;
                                    row.col(|ui| {
                                        ui.centered_and_justified(|ui| {
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
                                    });
                                    row.col(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&image.repository).strong(),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(&image.repository);
                                    });
                                    row.col(|ui| {
                                        ui.label(&image.tag);
                                    });
                                    row.col(|ui| {
                                        ui.label(RichText::new(&image.size).weak());
                                    });
                                    row.col(|ui| {
                                        ui.label(RichText::new(&image.created_at).weak());
                                    });
                                    row.col(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&image.id).monospace().weak(),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(&image.id);
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
            });
    }

    fn image_pull_modal(&mut self, ctx: &egui::Context) {
        if !self.image_pull_open {
            return;
        }
        egui::Modal::new("image_pull".into()).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.heading(RichText::new("拉取镜像").size(20.0));
            ui.add_space(4.0);
            ui.label(RichText::new("从镜像仓库下载镜像到本地。").weak());
            ui.add_space(16.0);

            ui.label(RichText::new("镜像名").strong());
            ui.add_space(4.0);
            let input = ui
                .add_enabled_ui(!self.image_pulling, |ui| {
                    ui.add_sized(
                        [ui.available_width(), 38.0],
                        egui::TextEdit::singleline(&mut self.image_pull_name)
                            .hint_text("例如：ubuntu:latest")
                            .font(egui::FontId::proportional(16.0))
                            .vertical_align(egui::Align::Center),
                    )
                })
                .inner;
            let submit_with_enter =
                input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("输出日志").strong());
                if self.image_pulling {
                    ui.spinner();
                    ui.label(
                        RichText::new("正在拉取")
                            .small()
                            .color(Color32::from_rgb(70, 135, 220)),
                    );
                }
            });
            ui.add_space(4.0);
            let dark_mode = ui.visuals().dark_mode;
            let terminal_fill = if dark_mode {
                Color32::from_rgb(20, 23, 28)
            } else {
                Color32::from_rgb(246, 248, 251)
            };
            let terminal_text = if dark_mode {
                Color32::from_rgb(215, 222, 232)
            } else {
                Color32::from_rgb(45, 52, 62)
            };
            egui::Frame::group(ui.style())
                .fill(terminal_fill)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .id_salt("image_pull_log")
                        .stick_to_bottom(true)
                        .max_height(280.0)
                        .show(ui, |ui| {
                            ui.set_min_height(240.0);
                            ui.set_min_width(ui.available_width());
                            let log = if self.image_pull_log.is_empty() {
                                "等待开始拉取..."
                            } else {
                                &self.image_pull_log
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(log).monospace().color(terminal_text),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                        });
                });

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can_pull =
                        !self.operation_running && !self.image_pull_name.trim().is_empty();
                    let pull_text = if self.image_pulling {
                        "拉取中..."
                    } else {
                        "开始拉取"
                    };
                    if ui
                        .add_enabled(
                            can_pull,
                            egui::Button::new(RichText::new(pull_text).color(Color32::WHITE))
                                .fill(Color32::from_rgb(50, 105, 190))
                                .min_size(egui::vec2(96.0, 32.0)),
                        )
                        .clicked()
                        || (submit_with_enter && can_pull)
                    {
                        self.pull_image(ctx.clone());
                    }
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(
                            !self.image_pulling,
                            egui::Button::new("关闭").min_size(egui::vec2(72.0, 32.0)),
                        )
                        .clicked()
                    {
                        self.image_pull_open = false;
                    }
                });
            });
        });
    }

    fn image_delete_confirm_modal(&mut self, ctx: &egui::Context) {
        if !self.image_delete_confirm_open {
            return;
        }
        if self.selected_images.is_empty() {
            self.image_delete_confirm_open = false;
            return;
        }

        egui::Modal::new("image_delete_confirm".into()).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(RichText::new("确认删除镜像").size(20.0));
            ui.add_space(6.0);
            ui.label(RichText::new("所选镜像将从本地存储中移除。").weak());
            ui.add_space(14.0);

            let warning_fill = if ui.visuals().dark_mode {
                Color32::from_rgb(55, 32, 34)
            } else {
                Color32::from_rgb(255, 241, 241)
            };
            egui::Frame::group(ui.style())
                .fill(warning_fill)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("!")
                                .size(22.0)
                                .strong()
                                .color(ui.visuals().error_fg_color),
                        );
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "即将删除 {} 个镜像",
                                    self.selected_images.len()
                                ))
                                .strong(),
                            );
                            ui.label(RichText::new("删除后无法撤销。请确保镜像不再需要。").weak());
                        });
                    });
                });

            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.operation_running,
                        egui::Button::new(RichText::new("确认删除").color(Color32::WHITE))
                            .fill(Color32::from_rgb(190, 55, 55))
                            .min_size(egui::vec2(92.0, 32.0)),
                    )
                    .clicked()
                {
                    self.image_delete_confirm_open = false;
                    self.delete_selected_images(ctx.clone());
                }
                ui.add_space(8.0);
                if ui
                    .add(egui::Button::new("取消").min_size(egui::vec2(72.0, 32.0)))
                    .clicked()
                {
                    self.image_delete_confirm_open = false;
                }
            });
        });
    }

    fn container_delete_confirm_modal(&mut self, ctx: &egui::Context) {
        if !self.container_delete_confirm_open {
            return;
        }
        let Some(container) = self
            .selected_container
            .as_ref()
            .and_then(|id| self.containers.iter().find(|container| &container.id == id))
            .cloned()
        else {
            self.container_delete_confirm_open = false;
            return;
        };

        egui::Modal::new("container_delete_confirm".into()).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(RichText::new("确认删除容器").size(20.0));
            ui.add_space(6.0);
            ui.label(RichText::new("所选容器及其中未持久化的数据将被移除。").weak());
            ui.add_space(14.0);

            let warning_fill = if ui.visuals().dark_mode {
                Color32::from_rgb(55, 32, 34)
            } else {
                Color32::from_rgb(255, 241, 241)
            };
            egui::Frame::group(ui.style())
                .fill(warning_fill)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("!")
                                .size(22.0)
                                .strong()
                                .color(ui.visuals().error_fg_color),
                        );
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(format!("即将删除容器 {}", container.names)).strong(),
                            );
                            let warning = if container.state.eq_ignore_ascii_case("running") {
                                "容器正在运行，将被强制停止并删除。"
                            } else {
                                "删除后无法撤销。请确保容器不再需要。"
                            };
                            ui.label(RichText::new(warning).weak());
                        });
                    });
                });

            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.operation_running,
                        egui::Button::new(RichText::new("确认删除").color(Color32::WHITE))
                            .fill(Color32::from_rgb(190, 55, 55))
                            .min_size(egui::vec2(92.0, 32.0)),
                    )
                    .clicked()
                {
                    self.container_delete_confirm_open = false;
                    self.delete_container(container.id.clone(), ctx.clone());
                }
                ui.add_space(8.0);
                if ui
                    .add(egui::Button::new("取消").min_size(egui::vec2(72.0, 32.0)))
                    .clicked()
                {
                    self.container_delete_confirm_open = false;
                }
            });
        });
    }

    fn container_create_modal(&mut self, ctx: &egui::Context) {
        if !self.container_create_open {
            return;
        }
        let image_options: Vec<_> = self.images.iter().map(ImageRow::command_target).collect();

        egui::Modal::new("container_create".into()).show(ctx, |ui| {
            ui.set_width(720.0);
            ui.heading(RichText::new("创建容器").size(20.0));
            ui.add_space(4.0);
            ui.label(
                RichText::new("根据 wslc run 参数配置并创建容器。多值参数每行填写一项。").weak(),
            );
            ui.add_space(12.0);

            ui.add_enabled_ui(!self.operation_running, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("container_create_form")
                    .max_height((ctx.content_rect().height() - 180.0).max(220.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.heading(RichText::new("基本设置").size(16.0));
                        ui.add_space(6.0);
                        ui.label(RichText::new("镜像名称 *").strong());
                        ui.add_enabled_ui(!image_options.is_empty(), |ui| {
                            egui::ComboBox::from_id_salt("container_create_image")
                                .selected_text(if self.container_create_form.image.is_empty() {
                                    "请选择本地镜像"
                                } else {
                                    &self.container_create_form.image
                                })
                                .width(ui.available_width())
                                .show_ui(ui, |ui| {
                                    for image in &image_options {
                                        ui.selectable_value(
                                            &mut self.container_create_form.image,
                                            image.clone(),
                                            image,
                                        );
                                    }
                                });
                        });
                        if image_options.is_empty() {
                            ui.label(
                                RichText::new("暂无本地镜像，请先到镜像列表拉取镜像。")
                                    .small()
                                    .color(ui.visuals().error_fg_color),
                            );
                        }
                        ui.add_space(6.0);
                        form_input(
                            ui,
                            "容器名称",
                            &mut self.container_create_form.name,
                            "--name",
                        );
                        form_input(
                            ui,
                            "启动命令",
                            &mut self.container_create_form.command,
                            "例如：/bin/bash",
                        );
                        form_multiline(
                            ui,
                            "命令参数",
                            &mut self.container_create_form.arguments,
                            "每行一个参数",
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.checkbox(
                                &mut self.container_create_form.detach,
                                "分离模式 (--detach)",
                            );
                            ui.checkbox(
                                &mut self.container_create_form.interactive,
                                "保持 Stdin 打开 (--interactive)",
                            );
                            ui.checkbox(&mut self.container_create_form.tty, "打开 TTY (--tty)");
                            ui.checkbox(
                                &mut self.container_create_form.remove,
                                "停止后移除 (--rm)",
                            );
                        });

                        ui.add_space(12.0);
                        egui::CollapsingHeader::new("网络与端口")
                            .default_open(true)
                            .show(ui, |ui| {
                                form_input(
                                    ui,
                                    "网络",
                                    &mut self.container_create_form.network,
                                    "--network",
                                );
                                form_multiline(
                                    ui,
                                    "发布端口",
                                    &mut self.container_create_form.publish,
                                    "例如：8080:80；每行一项",
                                );
                                ui.checkbox(
                                    &mut self.container_create_form.publish_all,
                                    "将所有公开端口随机发布 (--publish-all)",
                                );
                                form_multiline(
                                    ui,
                                    "DNS 服务器",
                                    &mut self.container_create_form.dns,
                                    "IP 地址；每行一项",
                                );
                                form_multiline(
                                    ui,
                                    "DNS 选项",
                                    &mut self.container_create_form.dns_option,
                                    "每行一项",
                                );
                                form_multiline(
                                    ui,
                                    "DNS 搜索域",
                                    &mut self.container_create_form.dns_search,
                                    "每行一项",
                                );
                                form_multiline(
                                    ui,
                                    "网络别名",
                                    &mut self.container_create_form.network_alias,
                                    "每行一项",
                                );
                                form_input(
                                    ui,
                                    "主机名",
                                    &mut self.container_create_form.hostname,
                                    "--hostname",
                                );
                                form_input(
                                    ui,
                                    "域名",
                                    &mut self.container_create_form.domainname,
                                    "--domainname",
                                );
                            });

                        ui.add_space(8.0);
                        egui::CollapsingHeader::new("环境与元数据").show(ui, |ui| {
                            form_multiline(
                                ui,
                                "环境变量",
                                &mut self.container_create_form.env,
                                "Key=Value；每行一项",
                            );
                            form_multiline(
                                ui,
                                "环境变量文件",
                                &mut self.container_create_form.env_file,
                                "文件路径；每行一项",
                            );
                            form_multiline(
                                ui,
                                "标签",
                                &mut self.container_create_form.label,
                                "Key=Value；每行一项",
                            );
                        });

                        ui.add_space(8.0);
                        egui::CollapsingHeader::new("资源限制").show(ui, |ui| {
                            form_input(
                                ui,
                                "CPU 数",
                                &mut self.container_create_form.cpus,
                                "例如：0.5、1、2.5",
                            );
                            form_input(
                                ui,
                                "内存限制",
                                &mut self.container_create_form.memory,
                                "例如：512M、1G",
                            );
                            form_input(
                                ui,
                                "共享内存大小",
                                &mut self.container_create_form.shm_size,
                                "例如：64M、1G",
                            );
                            form_input(
                                ui,
                                "GPU",
                                &mut self.container_create_form.gpus,
                                "例如：all",
                            );
                            form_multiline(
                                ui,
                                "Ulimit",
                                &mut self.container_create_form.ulimit,
                                "name=soft[:hard]；每行一项",
                            );
                        });

                        ui.add_space(8.0);
                        egui::CollapsingHeader::new("存储与工作目录").show(ui, |ui| {
                            form_multiline(
                                ui,
                                "卷挂载",
                                &mut self.container_create_form.volume,
                                "例如：主机路径:容器路径；每行一项",
                            );
                            form_multiline(
                                ui,
                                "tmpfs 挂载",
                                &mut self.container_create_form.tmpfs,
                                "容器路径；每行一项",
                            );
                            form_input(
                                ui,
                                "工作目录",
                                &mut self.container_create_form.workdir,
                                "容器内路径",
                            );
                        });

                        ui.add_space(8.0);
                        egui::CollapsingHeader::new("高级设置").show(ui, |ui| {
                            form_input(
                                ui,
                                "入口点",
                                &mut self.container_create_form.entrypoint,
                                "--entrypoint",
                            );
                            form_input(
                                ui,
                                "运行用户",
                                &mut self.container_create_form.user,
                                "name、uid 或 uid:gid",
                            );
                            form_input(
                                ui,
                                "停止信号",
                                &mut self.container_create_form.stop_signal,
                                "例如：SIGTERM",
                            );
                            form_input(
                                ui,
                                "容器 ID 文件",
                                &mut self.container_create_form.cidfile,
                                "写入容器 ID 的文件路径",
                            );
                        });
                    });
            });

            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button_text = if self.operation_running {
                    "创建中..."
                } else {
                    "创建容器"
                };
                if ui
                    .add_enabled(
                        !self.operation_running
                            && !self.container_create_form.image.trim().is_empty(),
                        egui::Button::new(RichText::new(button_text).color(Color32::WHITE))
                            .fill(Color32::from_rgb(50, 105, 190))
                            .min_size(egui::vec2(96.0, 32.0)),
                    )
                    .clicked()
                {
                    self.create_container(ctx.clone());
                }
                ui.add_space(8.0);
                if ui
                    .add_enabled(
                        !self.operation_running,
                        egui::Button::new("取消").min_size(egui::vec2(72.0, 32.0)),
                    )
                    .clicked()
                {
                    self.container_create_open = false;
                }
                if self.operation_running {
                    ui.spinner();
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
        let selected_container = self
            .selected_container
            .as_ref()
            .and_then(|id| self.containers.iter().find(|container| &container.id == id))
            .cloned();
        egui::Frame::group(ui.style())
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let search_width = ((ui.available_width() - 300.0) / 2.0).clamp(145.0, 190.0);
                    ui.add_sized(
                        [search_width, 34.0],
                        egui::TextEdit::singleline(&mut self.container_name_query)
                            .hint_text("容器名称")
                            .font(egui::FontId::proportional(14.0))
                            .vertical_align(egui::Align::Center),
                    );
                    ui.add_sized(
                        [search_width, 34.0],
                        egui::TextEdit::singleline(&mut self.container_image_query)
                            .hint_text("镜像名称")
                            .font(egui::FontId::proportional(14.0))
                            .vertical_align(egui::Align::Center),
                    );
                    ui.separator();

                    if ui
                        .add_enabled(
                            !self.operation_running,
                            egui::Button::new(RichText::new("创建").color(Color32::WHITE))
                                .fill(Color32::from_rgb(50, 105, 190))
                                .min_size(egui::vec2(76.0, 34.0)),
                        )
                        .clicked()
                    {
                        self.container_create_form = CreateContainerForm {
                            image: self
                                .images
                                .first()
                                .map(ImageRow::command_target)
                                .unwrap_or_default(),
                            ..Default::default()
                        };
                        self.container_create_open = true;
                    }

                    let can_operate = selected_container.is_some() && !self.operation_running;
                    let delete_color = ui.visuals().error_fg_color;
                    if ui
                        .add_enabled(
                            can_operate,
                            egui::Button::new(RichText::new("删除").color(delete_color))
                                .stroke(egui::Stroke::new(1.0, delete_color))
                                .min_size(egui::vec2(76.0, 34.0)),
                        )
                        .clicked()
                    {
                        self.container_delete_confirm_open = true;
                    }

                    let running = selected_container
                        .as_ref()
                        .is_some_and(|container| container.state.eq_ignore_ascii_case("running"));
                    if ui
                        .add_enabled(
                            can_operate && running,
                            egui::Button::new(RichText::new("Shell").color(Color32::WHITE))
                                .fill(Color32::from_rgb(50, 105, 190))
                                .min_size(egui::vec2(76.0, 34.0)),
                        )
                        .clicked()
                        && let Some(container) = &selected_container
                    {
                        self.open_container_shell(container.clone(), ui.ctx().clone());
                    }
                    if ui
                        .add_enabled(
                            can_operate,
                            egui::Button::new(
                                RichText::new(if running { "停止" } else { "启动" })
                                    .color(Color32::WHITE),
                            )
                            .fill(if running {
                                Color32::from_rgb(180, 110, 45)
                            } else {
                                Color32::from_rgb(50, 105, 190)
                            })
                            .min_size(egui::vec2(76.0, 34.0)),
                        )
                        .clicked()
                        && let Some(container) = &selected_container
                    {
                        if running {
                            self.stop_container(container.id.clone(), ui.ctx().clone());
                        } else {
                            self.start_container(container.id.clone(), ui.ctx().clone());
                        }
                    }
                    if self.operation_running {
                        ui.spinner();
                    }
                });
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
        if containers.is_empty() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(120.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(30.0);
                    let message = if self.containers_loading {
                        "正在加载容器..."
                    } else if self.containers.is_empty() {
                        "暂无容器"
                    } else {
                        "没有符合筛选条件的容器"
                    };
                    ui.label(RichText::new(message).size(15.0).weak());
                    if !self.containers_loading && self.containers.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new("点击上方“创建”运行新容器").small().weak());
                    }
                });
            });
            return;
        }

        egui::Frame::group(ui.style())
            .inner_margin(0.0)
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("containers_horizontal_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(1_100.0);
                        let header_color = ui.visuals().weak_text_color();
                        let shell_ctx = ui.ctx().clone();
                        TableBuilder::new(ui)
                            .striped(true)
                            .resizable(true)
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::initial(220.0).at_least(150.0))
                            .column(Column::initial(180.0).at_least(130.0))
                            .column(Column::initial(300.0).at_least(180.0))
                            .column(Column::initial(260.0).at_least(160.0))
                            .column(Column::remainder().at_least(300.0))
                            .header(38.0, |mut header| {
                                for title in ["容器名称", "状态", "镜像名称", "端口", "容器 ID"]
                                {
                                    header.col(|ui| {
                                        ui.label(RichText::new(title).strong().color(header_color));
                                    });
                                }
                            })
                            .body(|mut body| {
                                for container in containers {
                                    body.row(40.0, |mut row| {
                                        row.set_selected(
                                            self.selected_container.as_ref() == Some(&container.id),
                                        );
                                        row.col(|ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&container.names).strong(),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(&container.names);
                                        });
                                        row.col(|ui| {
                                            let running =
                                                container.state.eq_ignore_ascii_case("running");
                                            let color = if running {
                                                Color32::from_rgb(43, 166, 102)
                                            } else {
                                                ui.visuals().weak_text_color()
                                            };
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&container.status).color(color),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(&container.status);
                                        });
                                        row.col(|ui| {
                                            ui.add(egui::Label::new(&container.image).truncate())
                                                .on_hover_text(&container.image);
                                        });
                                        row.col(|ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&container.ports).monospace(),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(&container.ports);
                                        });
                                        row.col(|ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&container.id).monospace().weak(),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(&container.id);
                                        });
                                        let response = row.response();
                                        if response.clicked() || response.secondary_clicked() {
                                            self.selected_container = Some(container.id.clone());
                                        }
                                        if container_context_menu(&response, &container) {
                                            self.open_container_shell(
                                                container.clone(),
                                                shell_ctx.clone(),
                                            );
                                        }
                                    });
                                }
                            });
                    });
            });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("设置");
        ui.add_space(12.0);
        egui::Frame::group(ui.style())
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("暂无可配置项").weak());
            });
    }

    fn open_container_shell(&mut self, container: ContainerRow, ctx: egui::Context) {
        if !container.state.eq_ignore_ascii_case("running") {
            self.status = Some(("只有运行中的容器可以进入 Shell".to_owned(), true));
            return;
        }

        self.terminal = None;
        match TerminalSession::start(
            &wslc_executable(),
            &container.id,
            container.names.clone(),
            ctx,
        ) {
            Ok(terminal) => {
                self.terminal = Some(terminal);
                self.page = Page::Shell;
                self.status = Some((format!("已进入容器 {} 的 Shell", container.names), false));
            }
            Err(error) => self.status = Some((error, true)),
        }
    }

    fn shell_page(&mut self, ui: &mut egui::Ui) {
        let Some(terminal) = self.terminal.as_mut() else {
            self.page = Page::Containers;
            return;
        };

        let mut close = false;
        ui.horizontal(|ui| {
            ui.heading(format!("容器 Shell · {}", terminal.container_name));
            ui.add_space(8.0);
            if terminal.is_running() {
                ui.spinner();
                ui.label(RichText::new("已连接").color(Color32::from_rgb(43, 166, 102)));
            } else if let Some(message) = terminal.exit_message() {
                ui.label(RichText::new(message).color(ui.visuals().weak_text_color()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("关闭 Shell").clicked() {
                    close = true;
                }
                ui.label(
                    RichText::new("拖动选择，Ctrl+Shift+C 复制，Ctrl+V 粘贴")
                        .small()
                        .weak(),
                );
            });
        });
        ui.add_space(10.0);
        terminal.show(ui);

        if close {
            self.terminal = None;
            self.page = Page::Containers;
            self.status = Some(("Shell 已关闭".to_owned(), false));
        }
    }
}

impl ImageRow {
    fn key(&self) -> String {
        format!("{}\0{}\0{}", self.repository, self.tag, self.id)
    }

    fn command_target(&self) -> String {
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
            ports: format_ports(&container.ports),
        }
    }
}

fn format_ports(ports: &[WslcPort]) -> String {
    if ports.is_empty() {
        return "-".to_owned();
    }

    ports
        .iter()
        .map(|port| {
            let protocol = match port.protocol {
                6 => "tcp".to_owned(),
                17 => "udp".to_owned(),
                value => value.to_string(),
            };
            let address = if port.binding_address.contains(':') {
                format!("[{}]", port.binding_address)
            } else {
                port.binding_address.clone()
            };
            format!(
                "{address}:{} -> {}/{}",
                port.host_port, port.container_port, protocol
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
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
                Page::Shell => self.shell_page(ui),
                Page::Settings => self.settings_page(ui),
            }
        });
        self.image_delete_confirm_modal(ui.ctx());
        self.container_delete_confirm_modal(ui.ctx());
        self.container_create_modal(ui.ctx());
        self.image_pull_modal(ui.ctx());
        self.update_modal(ui.ctx());
        self.wslc_install_modal(ui.ctx());
    }
}

fn form_input(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(RichText::new(label).strong());
    ui.add_sized(
        [ui.available_width(), 32.0],
        egui::TextEdit::singleline(value).hint_text(hint),
    );
    ui.add_space(6.0);
}

fn form_multiline(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(RichText::new(label).strong());
    ui.add_sized(
        [ui.available_width(), 58.0],
        egui::TextEdit::multiline(value).hint_text(hint),
    );
    ui.add_space(6.0);
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

fn container_context_menu(response: &egui::Response, container: &ContainerRow) -> bool {
    let mut open_shell = false;
    response.context_menu(|ui| {
        if ui
            .add_enabled(
                container.state.eq_ignore_ascii_case("running"),
                egui::Button::new("进入 Shell"),
            )
            .clicked()
        {
            open_shell = true;
            ui.close();
        }
        ui.separator();
        if ui.button("复制容器名称").clicked() {
            ui.ctx().copy_text(container.names.clone());
            ui.close();
        }
        if ui.button("复制容器 ID").clicked() {
            ui.ctx().copy_text(container.id.clone());
            ui.close();
        }
    });
    open_shell
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
    run_wslc_args(args)
}

fn run_wslc_args<I, S>(args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
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

fn run_wslc_pull(image: &str, tx: Sender<Message>, ctx: egui::Context) -> Result<(), String> {
    let mut child = Command::new(wslc_executable())
        .args(["pull", image])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法运行 wslc，请确认 WSLC 已安装：{error}"))?;

    let stdout = child.stdout.take().expect("piped stdout is available");
    let stderr = child.stderr.take().expect("piped stderr is available");
    let stdout_tx = tx.clone();
    let stdout_ctx = ctx.clone();
    let stdout_thread = thread::spawn(move || {
        forward_pull_output(stdout, stdout_tx, stdout_ctx);
    });
    let stderr_thread = thread::spawn(move || {
        forward_pull_output(stderr, tx, ctx);
    });

    let status = child
        .wait()
        .map_err(|error| format!("等待 WSLC 拉取命令时出错：{error}"))?;
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    if status.success() {
        Ok(())
    } else {
        Err(format!("镜像拉取失败（{status}）"))
    }
}

fn forward_pull_output(reader: impl Read, tx: Sender<Message>, ctx: egui::Context) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {
                let output = decode_command_output(&buffer);
                if !output.is_empty() {
                    let _ = tx.send(Message::ImagePullOutput(output));
                    ctx.request_repaint();
                }
            }
            Err(error) => {
                let _ = tx.send(Message::ImagePullOutput(format!(
                    "\n读取命令输出失败：{error}\n"
                )));
                ctx.request_repaint();
                break;
            }
        }
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
    get_wslc_version().is_some()
}

fn get_wslc_version() -> Option<String> {
    Command::new(wslc_executable())
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let version = String::from_utf8_lossy(&output.stdout)
                .trim()
                .strip_prefix("wslc ")
                .unwrap_or_default()
                .trim()
                .to_owned();
            (!version.is_empty()).then_some(version)
        })
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
    fn parses_and_formats_container_ports() {
        let container: WslcContainer = serde_json::from_str(
            r#"{
                "Id": "container-id",
                "Name": "web",
                "Image": "example/web:latest",
                "State": 2,
                "StateChangedAt": 0,
                "Ports": [
                    {
                        "BindingAddress": "127.0.0.1",
                        "ContainerPort": 3000,
                        "HostPort": 8080,
                        "Protocol": 6
                    },
                    {
                        "BindingAddress": "::1",
                        "ContainerPort": 53,
                        "HostPort": 53,
                        "Protocol": 17
                    }
                ]
            }"#,
        )
        .unwrap();

        let row = ContainerRow::from(container);
        assert_eq!(row.ports, "127.0.0.1:8080 -> 3000/tcp, [::1]:53 -> 53/udp");
    }

    #[test]
    fn builds_container_run_arguments() {
        let form = CreateContainerForm {
            image: " example/web:latest ".to_owned(),
            command: "/bin/sh".to_owned(),
            arguments: "-c\necho hello\n".to_owned(),
            cpus: "0.5".to_owned(),
            detach: true,
            env: "MODE=prod\nDEBUG=false".to_owned(),
            publish: "8080:80\n8443:443".to_owned(),
            remove: true,
            name: "web".to_owned(),
            ..Default::default()
        };

        assert_eq!(
            form.to_args().unwrap(),
            [
                "run",
                "--cpus",
                "0.5",
                "--detach",
                "--env",
                "MODE=prod",
                "--env",
                "DEBUG=false",
                "--name",
                "web",
                "--publish",
                "8080:80",
                "--publish",
                "8443:443",
                "--rm",
                "example/web:latest",
                "/bin/sh",
                "-c",
                "echo hello",
            ]
        );
    }

    #[test]
    fn validates_container_run_arguments() {
        assert!(CreateContainerForm::default().to_args().is_err());
        let invalid_cpus = CreateContainerForm {
            image: "ubuntu".to_owned(),
            cpus: "zero".to_owned(),
            ..Default::default()
        };
        assert!(invalid_cpus.to_args().is_err());
        let non_finite_cpus = CreateContainerForm {
            image: "ubuntu".to_owned(),
            cpus: "NaN".to_owned(),
            ..Default::default()
        };
        assert!(non_finite_cpus.to_args().is_err());
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
