use std::{
    io::{Read, Write},
    path::Path,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use eframe::egui::{
    self, Align2, Color32, Event, FontFamily, FontId, Key, Modifiers, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

const INITIAL_ROWS: u16 = 30;
const INITIAL_COLS: u16 = 100;
const SCROLLBACK_ROWS: usize = 5_000;
const PADDING: f32 = 8.0;

enum TerminalEvent {
    Output(Vec<u8>),
    Exited(Result<u32, String>),
    Error(String),
}

#[derive(Clone, Copy)]
struct Selection {
    start: (u16, u16),
    end: (u16, u16),
}

#[derive(Default)]
struct TerminalCallbacks {
    responses: Vec<Vec<u8>>,
}

impl vt100::Callbacks for TerminalCallbacks {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        intermediate: Option<u8>,
        _second_intermediate: Option<u8>,
        params: &[&[u16]],
        command: char,
    ) {
        let parameter = params.first().and_then(|values| values.first()).copied();
        match (intermediate, parameter, command) {
            (None, Some(5), 'n') => self.responses.push(b"\x1b[0n".to_vec()),
            (None, Some(6), 'n') => {
                let (row, col) = screen.cursor_position();
                self.responses
                    .push(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
            }
            (Some(b'?'), Some(6), 'n') => {
                let (row, col) = screen.cursor_position();
                self.responses
                    .push(format!("\x1b[?{};{}R", row + 1, col + 1).into_bytes());
            }
            (None, None | Some(0), 'c') => self.responses.push(b"\x1b[?1;2c".to_vec()),
            _ => {}
        }
    }
}

pub struct TerminalSession {
    pub container_name: String,
    parser: vt100::Parser<TerminalCallbacks>,
    master: Box<dyn MasterPty + Send>,
    input_tx: Sender<Vec<u8>>,
    event_rx: Receiver<TerminalEvent>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    rows: u16,
    cols: u16,
    selection: Option<Selection>,
    exit_status: Option<Result<u32, String>>,
}

impl TerminalSession {
    pub fn start(
        executable: &Path,
        container_id: &str,
        container_name: String,
        ctx: egui::Context,
    ) -> Result<Self, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(pty_size(INITIAL_ROWS, INITIAL_COLS, 0.0, 0.0))
            .map_err(|error| format!("无法创建容器 Shell 终端：{error}"))?;

        let mut command = CommandBuilder::new(executable);
        command.args(["exec", "--interactive", "--tty", container_id, "/bin/sh"]);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("无法读取容器 Shell：{error}"))?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("无法写入容器 Shell：{error}"))?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("无法启动容器 Shell：{error}"))?;
        drop(pair.slave);

        let killer = child.clone_killer();
        let (event_tx, event_rx) = mpsc::channel();
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();

        let reader_tx = event_tx.clone();
        let reader_ctx = ctx.clone();
        thread::spawn(move || {
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if reader_tx
                            .send(TerminalEvent::Output(buffer[..read].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                        reader_ctx.request_repaint();
                    }
                    Err(error) => {
                        let _ = reader_tx.send(TerminalEvent::Error(format!(
                            "读取容器 Shell 失败：{error}"
                        )));
                        reader_ctx.request_repaint();
                        break;
                    }
                }
            }
        });

        let writer_tx = event_tx.clone();
        let writer_ctx = ctx.clone();
        thread::spawn(move || {
            while let Ok(input) = input_rx.recv() {
                if let Err(error) = writer.write_all(&input).and_then(|()| writer.flush()) {
                    let _ = writer_tx.send(TerminalEvent::Error(format!(
                        "写入容器 Shell 失败：{error}"
                    )));
                    writer_ctx.request_repaint();
                    break;
                }
            }
        });

        thread::spawn(move || {
            let result = child
                .wait()
                .map(|status| status.exit_code())
                .map_err(|error| format!("等待容器 Shell 退出时出错：{error}"));
            let _ = event_tx.send(TerminalEvent::Exited(result));
            ctx.request_repaint();
        });

        Ok(Self {
            container_name,
            parser: vt100::Parser::new_with_callbacks(
                INITIAL_ROWS,
                INITIAL_COLS,
                SCROLLBACK_ROWS,
                TerminalCallbacks::default(),
            ),
            master: pair.master,
            input_tx,
            event_rx,
            killer,
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            selection: None,
            exit_status: None,
        })
    }

    pub fn is_running(&self) -> bool {
        self.exit_status.is_none()
    }

    pub fn exit_message(&self) -> Option<String> {
        self.exit_status.as_ref().map(|result| match result {
            Ok(0) => "Shell 已退出".to_owned(),
            Ok(code) => format!("Shell 已退出，代码 {code}"),
            Err(error) => error.clone(),
        })
    }

    pub fn show(&mut self, ui: &mut Ui) -> Response {
        self.receive_events();

        let font = FontId::new(14.0, FontFamily::Monospace);
        let sample = ui
            .painter()
            .layout_no_wrap("M".to_owned(), font.clone(), Color32::WHITE);
        let cell_width = sample.size().x.max(7.0);
        let cell_height = (sample.size().y + 3.0).max(16.0);
        let available = ui.available_size().max(Vec2::new(160.0, 100.0));
        let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
        let content_rect = rect.shrink(PADDING);
        let cols = ((content_rect.width() / cell_width).floor() as u16).max(2);
        let rows = ((content_rect.height() / cell_height).floor() as u16).max(2);
        self.resize(rows, cols, cell_width, cell_height);

        ui.painter()
            .rect_filled(rect, 4.0, Color32::from_rgb(19, 22, 27));
        if response.clicked() {
            response.request_focus();
        }
        self.handle_pointer(ui, &response, content_rect, cell_width, cell_height);
        if response.has_focus() {
            self.handle_input(ui);
        }
        self.paint(ui, content_rect, cell_width, cell_height, font);

        response
    }

    fn receive_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                TerminalEvent::Output(bytes) => {
                    if self.parser.screen().scrollback() == 0 {
                        self.parser.process(&bytes);
                    } else {
                        let scrollback = self.parser.screen().scrollback();
                        self.parser.process(&bytes);
                        self.parser.screen_mut().set_scrollback(scrollback);
                    }
                    for response in std::mem::take(&mut self.parser.callbacks_mut().responses) {
                        self.send(response);
                    }
                }
                TerminalEvent::Exited(result) => self.exit_status = Some(result),
                TerminalEvent::Error(error) => self
                    .parser
                    .process(format!("\r\n[DevDock] {error}\r\n").as_bytes()),
            }
        }
    }

    fn resize(&mut self, rows: u16, cols: u16, cell_width: f32, cell_height: f32) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        let size = pty_size(rows, cols, cell_width, cell_height);
        if self.master.resize(size).is_ok() {
            self.parser.screen_mut().set_size(rows, cols);
            self.rows = rows;
            self.cols = cols;
            self.selection = None;
        }
    }

    fn handle_input(&mut self, ui: &Ui) {
        let events = ui.input(|input| input.events.clone());
        for event in events {
            match event {
                Event::Text(text) if !text.is_empty() => self.send(text.into_bytes()),
                Event::Paste(text) => {
                    if self.parser.screen().bracketed_paste() {
                        self.send(format!("\x1b[200~{text}\x1b[201~").into_bytes());
                    } else {
                        self.send(text.into_bytes());
                    }
                }
                Event::Copy => {
                    if ui.input(|input| input.modifiers.shift)
                        && let Some(text) = self.selected_text()
                    {
                        ui.ctx().copy_text(text);
                    }
                }
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if key == Key::V && modifiers.ctrl {
                        continue;
                    }
                    if let Some(input) = key_input(key, modifiers, self.parser.screen()) {
                        self.send(input);
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_pointer(
        &mut self,
        ui: &Ui,
        response: &Response,
        rect: Rect,
        cell_width: f32,
        cell_height: f32,
    ) {
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let current = self.parser.screen().scrollback();
                let lines = (scroll.abs() / cell_height).ceil().max(1.0) as usize;
                let target = if scroll > 0.0 {
                    current.saturating_add(lines)
                } else {
                    current.saturating_sub(lines)
                };
                self.parser.screen_mut().set_scrollback(target);
            }
        }

        if response.drag_started()
            && let Some(position) = response.interact_pointer_pos()
        {
            let cell = pointer_cell(
                position,
                rect,
                cell_width,
                cell_height,
                self.rows,
                self.cols,
            );
            self.selection = Some(Selection {
                start: cell,
                end: cell,
            });
        }
        if response.dragged()
            && let (Some(position), Some(selection)) =
                (response.interact_pointer_pos(), self.selection.as_mut())
        {
            selection.end = pointer_cell(
                position,
                rect,
                cell_width,
                cell_height,
                self.rows,
                self.cols,
            );
        }
    }

    fn paint(&self, ui: &Ui, rect: Rect, cell_width: f32, cell_height: f32, font: FontId) {
        let painter = ui.painter().with_clip_rect(rect);
        let screen = self.parser.screen();
        for row in 0..self.rows {
            for col in 0..self.cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let position = Pos2::new(
                    rect.left() + f32::from(col) * cell_width,
                    rect.top() + f32::from(row) * cell_height,
                );
                let width = if cell.is_wide() {
                    cell_width * 2.0
                } else {
                    cell_width
                };
                let cell_rect = Rect::from_min_size(position, Vec2::new(width, cell_height));
                let mut foreground = terminal_color(cell.fgcolor(), true);
                let mut background = terminal_color(cell.bgcolor(), false);
                if cell.inverse() {
                    std::mem::swap(&mut foreground, &mut background);
                }
                if background != Color32::from_rgb(19, 22, 27) {
                    painter.rect_filled(cell_rect, 0.0, background);
                }
                if self.cell_selected(row, col) {
                    painter.rect_filled(
                        cell_rect,
                        0.0,
                        Color32::from_rgba_unmultiplied(70, 125, 200, 145),
                    );
                }
                if cell.has_contents() {
                    if cell.dim() {
                        foreground = foreground.gamma_multiply(0.65);
                    }
                    painter.text(
                        position,
                        Align2::LEFT_TOP,
                        cell.contents(),
                        font.clone(),
                        foreground,
                    );
                    if cell.underline() {
                        painter.line_segment(
                            [cell_rect.left_bottom(), cell_rect.right_bottom()],
                            Stroke::new(1.0, foreground),
                        );
                    }
                }
            }
        }

        if !screen.hide_cursor() && screen.scrollback() == 0 && self.is_running() {
            let (row, col) = screen.cursor_position();
            let cursor = Rect::from_min_size(
                Pos2::new(
                    rect.left() + f32::from(col) * cell_width,
                    rect.top() + f32::from(row) * cell_height,
                ),
                Vec2::new(cell_width, cell_height),
            );
            painter.rect_stroke(
                cursor,
                0.0,
                Stroke::new(1.5, Color32::from_rgb(215, 222, 232)),
                StrokeKind::Inside,
            );
        }
    }

    fn send(&self, input: Vec<u8>) {
        if self.is_running() {
            let _ = self.input_tx.send(input);
        }
    }

    fn cell_selected(&self, row: u16, col: u16) -> bool {
        self.selection.is_some_and(|selection| {
            let (start, end) = ordered_selection(selection);
            (row, col) >= start && (row, col) <= end
        })
    }

    fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let (start, end) = ordered_selection(selection);
        Some(self.parser.screen().contents_between(
            start.0,
            start.1,
            end.0,
            end.1.saturating_add(1),
        ))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.is_running() {
            let _ = self.killer.kill();
        }
    }
}

fn pty_size(rows: u16, cols: u16, cell_width: f32, cell_height: f32) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: (f32::from(cols) * cell_width).round() as u16,
        pixel_height: (f32::from(rows) * cell_height).round() as u16,
    }
}

fn pointer_cell(
    position: Pos2,
    rect: Rect,
    cell_width: f32,
    cell_height: f32,
    rows: u16,
    cols: u16,
) -> (u16, u16) {
    let col = ((position.x - rect.left()) / cell_width).floor().max(0.0) as u16;
    let row = ((position.y - rect.top()) / cell_height).floor().max(0.0) as u16;
    (row.min(rows - 1), col.min(cols - 1))
}

fn ordered_selection(selection: Selection) -> ((u16, u16), (u16, u16)) {
    if selection.start <= selection.end {
        (selection.start, selection.end)
    } else {
        (selection.end, selection.start)
    }
}

fn key_input(key: Key, modifiers: Modifiers, screen: &vt100::Screen) -> Option<Vec<u8>> {
    if modifiers.ctrl
        && !modifiers.shift
        && let Some(control) = control_byte(key)
    {
        return Some(vec![control]);
    }

    let application = screen.application_cursor();
    let input: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Tab if modifiers.shift => b"\x1b[Z",
        Key::Tab => b"\t",
        Key::Backspace => b"\x7f",
        Key::Escape => b"\x1b",
        Key::ArrowUp if application => b"\x1bOA",
        Key::ArrowDown if application => b"\x1bOB",
        Key::ArrowRight if application => b"\x1bOC",
        Key::ArrowLeft if application => b"\x1bOD",
        Key::ArrowUp => b"\x1b[A",
        Key::ArrowDown => b"\x1b[B",
        Key::ArrowRight => b"\x1b[C",
        Key::ArrowLeft => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::Delete => b"\x1b[3~",
        Key::Insert => b"\x1b[2~",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        _ => return None,
    };
    Some(input.to_vec())
}

fn control_byte(key: Key) -> Option<u8> {
    let byte = match key {
        Key::A => 0x01,
        Key::B => 0x02,
        Key::C => 0x03,
        Key::D => 0x04,
        Key::E => 0x05,
        Key::F => 0x06,
        Key::G => 0x07,
        Key::H => 0x08,
        Key::I => 0x09,
        Key::J => 0x0a,
        Key::K => 0x0b,
        Key::L => 0x0c,
        Key::M => 0x0d,
        Key::N => 0x0e,
        Key::O => 0x0f,
        Key::P => 0x10,
        Key::Q => 0x11,
        Key::R => 0x12,
        Key::S => 0x13,
        Key::T => 0x14,
        Key::U => 0x15,
        Key::V => 0x16,
        Key::W => 0x17,
        Key::X => 0x18,
        Key::Y => 0x19,
        Key::Z => 0x1a,
        _ => return None,
    };
    Some(byte)
}

fn terminal_color(color: vt100::Color, foreground: bool) -> Color32 {
    match color {
        vt100::Color::Default if foreground => Color32::from_rgb(215, 222, 232),
        vt100::Color::Default => Color32::from_rgb(19, 22, 27),
        vt100::Color::Rgb(red, green, blue) => Color32::from_rgb(red, green, blue),
        vt100::Color::Idx(index) => indexed_color(index),
    }
}

fn indexed_color(index: u8) -> Color32 {
    const ANSI: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    if let Some(&(red, green, blue)) = ANSI.get(usize::from(index)) {
        return Color32::from_rgb(red, green, blue);
    }
    if index < 232 {
        let value = index - 16;
        let component = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
        return Color32::from_rgb(
            component(value / 36),
            component((value % 36) / 6),
            component(value % 6),
        );
    }
    let gray = 8 + (index - 232) * 10;
    Color32::from_gray(gray)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_control_and_navigation_keys() {
        let screen = vt100::Parser::default();
        assert_eq!(
            key_input(Key::C, Modifiers::CTRL, screen.screen()),
            Some(vec![3])
        );
        assert_eq!(
            key_input(Key::ArrowUp, Modifiers::NONE, screen.screen()),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn maps_xterm_color_cube() {
        assert_eq!(indexed_color(16), Color32::BLACK);
        assert_eq!(indexed_color(231), Color32::WHITE);
        assert_eq!(indexed_color(232), Color32::from_gray(8));
    }

    #[test]
    fn responds_to_terminal_status_queries() {
        let mut parser = vt100::Parser::new_with_callbacks(24, 80, 0, TerminalCallbacks::default());
        parser.process(b"\x1b[4;8H\x1b[5n\x1b[6n\x1b[c");
        assert_eq!(
            parser.callbacks().responses,
            [
                b"\x1b[0n".to_vec(),
                b"\x1b[4;8R".to_vec(),
                b"\x1b[?1;2c".to_vec(),
            ]
        );
    }
}
