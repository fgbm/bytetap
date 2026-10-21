//! Шаг сценария — форма одного из трёх режимов (FR-1, FR-2, FR-9, FR-11, FR-14,
//! U1-AC10…AC12), как `ScenarioForm.vue`.
//!
//! Поля минимальные: подпись 12px над полем, само поле 34px с тонкой рамкой; служебное
//! (интерфейс, переподключение, поток, DTR/RTS, анализатор, комментарий, папка журналов,
//! NAK, профили) — под свёрнутым «Дополнительно». Ошибка показывается красным текстом
//! 12px под своим полем, без баннеров.

use eframe::egui::{
    self, Align, Color32, CursorIcon, Frame, Id, Key, Layout, Rect, RichText, Role, Sense, Stroke,
    Ui, UiBuilder, Vec2, WidgetInfo, vec2,
};

use crate::app::{BytetapApp, Screen, centered};
use crate::errors::ErrorField;
use crate::form::{FORMATS, apply_format, check_baud, check_port, parse_reconnect, serial_format};
use crate::icons::{self, Icon};
use crate::scenarios::mode_title;
use crate::settings::{
    ALL_INTERFACES, BAUD_RATES, Flow, Responder, SessionMode, remove_profile, upsert_profile,
};
use crate::theme::tokens::{
    RADIUS_PANEL, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_5, TEXT_BASE, TEXT_XL, TEXT_XS,
};
use crate::theme::{self, Palette, radius};
use crate::widgets::{self, FieldLook};
use crate::{diag, t};

/// Ширина колонки формы: 560px минус поля по 24px.
const FORM_WIDTH: f32 = 512.0;
/// Сколько «Скопировано» держится на кнопке, секунды.
const COPIED_FOR: f64 = 1.5;

const RESPONDERS: [Responder; 4] = [
    Responder::Auto,
    Responder::Hl7,
    Responder::Astm,
    Responder::None,
];

fn responder_text(responder: Responder) -> &'static str {
    match responder {
        Responder::Auto => t!("form.responderAuto"),
        Responder::Hl7 => t!("form.responderHl7"),
        Responder::Astm => t!("form.responderAstm"),
        Responder::None => t!("form.responderNone"),
    }
}

const FLOWS: [Flow; 3] = [Flow::None, Flow::Rtscts, Flow::Xonxoff];

fn flow_text(flow: Flow) -> &'static str {
    match flow {
        Flow::None => t!("form.flowNone"),
        Flow::Rtscts => t!("form.flowRtscts"),
        Flow::Xonxoff => t!("form.flowXonxoff"),
    }
}

/// Поле, под которым видна ошибка в этом режиме: поля чужого режима на экране нет,
/// тогда ошибка встаёт под «Старт». Адрес у сервера — интерфейс прослушивания.
pub fn shown_field(field: ErrorField, mode: SessionMode) -> ErrorField {
    match (field, mode) {
        (ErrorField::Port | ErrorField::Address, SessionMode::Serial)
        | (ErrorField::Serial, SessionMode::TcpServer | SessionMode::TcpClient) => {
            ErrorField::Start
        }
        (field, _) => field,
    }
}

/// Поле с подписью сверху и ошибкой снизу.
fn field(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    error: Option<&str>,
    control: impl FnOnce(&mut Ui, &egui::Response),
) {
    let label = widgets::field_label(ui, p, label);
    ui.add_space(SPACE_1);
    control(ui, &label);
    if let Some(error) = error {
        ui.add_space(SPACE_1);
        widgets::field_error(ui, p.danger, error);
    }
}

/// Ширина кнопки-пилюли с подписью: место под неё справа от поля.
fn chip_width(ui: &Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), theme::sans(TEXT_XS), Color32::WHITE)
        .size()
        .x
        + 20.0
}

impl BytetapApp {
    /// Две колонки с промежутком 16px; высота строки — по более высокой колонке.
    fn row(
        &mut self,
        ui: &mut Ui,
        left: impl FnOnce(&mut Self, &mut Ui),
        right: impl FnOnce(&mut Self, &mut Ui),
    ) {
        let width = ui.available_width();
        let column = ((width - SPACE_4) / 2.0).floor();
        let top = ui.cursor().min;
        let child = |ui: &mut Ui, x: f32| {
            ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        top + vec2(x, 0.0),
                        vec2(column, 4000.0),
                    ))
                    .layout(Layout::top_down(Align::Min)),
            )
        };
        let mut left_ui = child(ui, 0.0);
        left(self, &mut left_ui);
        let mut right_ui = child(ui, width - column);
        right(self, &mut right_ui);
        let used = left_ui.min_rect().union(right_ui.min_rect());
        ui.advance_cursor_after_rect(used);
    }

    /// Текст ошибки у поля: сначала своя проверка, затем ошибка старта по коду.
    fn error_text(&self, field: ErrorField) -> Option<String> {
        let cfg = &self.settings.config;
        if let Some(own) = self.form.local_error(cfg, field) {
            return Some(own.to_owned());
        }
        let error = self.start_error.as_ref()?;
        (shown_field(error.field(cfg.mode), cfg.mode) == field).then(|| error.text(cfg))
    }

    pub(crate) fn step(&mut self, ui: &mut Ui, mode: SessionMode) {
        let ctx = ui.ctx().clone();
        let p = self.pal();
        let mut back = false;
        let mut start = false;
        centered(ui, FORM_WIDTH, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(SPACE_4);
            back = widgets::link_button(ui, &p, Icon::ArrowLeft, 15.0, t!("form.back"), false)
                .clicked();
            ui.add_space(SPACE_3);
            ui.label(
                RichText::new(mode_title(mode))
                    .font(theme::sans_semibold(18.0))
                    .color(p.text),
            );
            ui.add_space(SPACE_4);
            match mode {
                SessionMode::TcpServer => self.server_fields(ui, &p),
                SessionMode::TcpClient => self.client_fields(ui, &p),
                SessionMode::Serial => self.serial_fields(ui, &p),
            }
            self.more(ui, &p, mode);
            ui.add_space(SPACE_4);
            start = widgets::start_button(ui, &p, t!("form.start"), true).clicked();
            if let Some(error) = self.error_text(ErrorField::Start) {
                ui.add_space(SPACE_2);
                ui.vertical_centered(|ui| widgets::field_error(ui, p.danger, &error));
            }
            ui.add_space(SPACE_5);
        });
        if back {
            self.back_to_scenarios(&ctx);
        } else if start {
            self.start();
        }
    }

    fn responder_field(&mut self, ui: &mut Ui, p: &Palette) {
        field(ui, p, t!("form.responder"), None, |ui, _| {
            let options: Vec<String> = RESPONDERS
                .iter()
                .map(|r| responder_text(*r).to_owned())
                .collect();
            let current = RESPONDERS
                .iter()
                .position(|r| *r == self.settings.config.responder);
            let (_, chosen) = widgets::select(
                ui,
                p,
                Id::unique("bt-responder"),
                t!("form.responder"),
                &options,
                current,
                FieldLook::default(),
            );
            if let Some(index) = chosen {
                self.settings_mut().config.responder = RESPONDERS[index];
            }
        });
    }

    /// (а) Анализатор подключается ко мне: порт, автоответ, адрес для анализатора.
    fn server_fields(&mut self, ui: &mut Ui, p: &Palette) {
        self.row(
            ui,
            |app, ui| {
                let error = app.error_text(ErrorField::Port);
                field(ui, p, t!("form.port"), error.as_deref(), |ui, label| {
                    let look = FieldLook {
                        mono: true,
                        error: error.is_some(),
                        ..FieldLook::default()
                    };
                    let id = Id::unique("bt-port");
                    let response =
                        widgets::text_input(ui, p, id, &mut app.form.port_text, label, "", look);
                    if response.changed() {
                        app.form.touched.port = true;
                        if let Ok(port) = check_port(&app.form.port_text) {
                            app.settings_mut().config.listen_port = port;
                        }
                    }
                });
            },
            |_, _| {},
        );
        ui.add_space(SPACE_3);
        self.responder_field(ui, p);
        ui.add_space(SPACE_3 + SPACE_4);
        self.address_block(ui, p);
    }

    /// Адрес, который вписывается в анализатор: адрес ноутбука и порт, крупно.
    fn address_block(&mut self, ui: &mut Ui, p: &Palette) {
        let interfaces = self.interfaces.ready().cloned().unwrap_or_default();
        let cfg = &self.settings.config;
        let host = if cfg.listen_host.is_empty() {
            interfaces
                .first()
                .map_or_else(|| "0.0.0.0".to_owned(), |iface| iface.addr.clone())
        } else {
            cfg.listen_host.clone()
        };
        let port = check_port(&self.form.port_text)
            .map_or_else(|_| self.form.port_text.clone(), |port| port.to_string());
        let address = format!("{host}:{port}");
        let now = ui.input(|i| i.time);
        let copied = self.copied_until.is_some_and(|until| now < until);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SPACE_3;
            ui.label(
                RichText::new(&address)
                    .font(theme::mono_medium(TEXT_XL))
                    .extra_letter_spacing(-0.28)
                    .color(p.text),
            );
            let text = if copied {
                t!("form.copied")
            } else {
                t!("form.copy")
            };
            if widgets::chip_button(ui, p, Some(Icon::Copy), text, true).clicked() {
                ui.ctx().copy_text(address.clone());
                self.copied_until = Some(now + COPIED_FOR);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(COPIED_FOR));
            }
        });
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.y = SPACE_1;
        if let Some(other) = interfaces.iter().find(|iface| iface.addr != host) {
            widgets::hint(
                ui,
                p,
                &t!("form.otherNetwork", addr = other.addr, name = other.name),
            );
        } else if self.interfaces.ready().is_some_and(Vec::is_empty) {
            widgets::hint(ui, p, t!("form.noInterfaces"));
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            widgets::hint(ui, p, t!("form.firewall"));
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            response
                .widget_info(|| WidgetInfo::labeled(Role::Image, true, t!("form.firewallHint")));
            icons::paint(ui, Icon::Info, rect.center(), 13.0, p.text_faint);
            response
                .on_hover_cursor(CursorIcon::Help)
                .on_hover_text(t!("form.firewallHint"));
        });
        ui.spacing_mut().item_spacing.y = 0.0;
    }

    /// (б) Я подключаюсь к анализатору: адрес, порт, автоответ.
    fn client_fields(&mut self, ui: &mut Ui, p: &Palette) {
        let error = self.error_text(ErrorField::Address);
        field(
            ui,
            p,
            t!("form.connectAddress"),
            error.as_deref(),
            |ui, label| {
                let look = FieldLook {
                    mono: true,
                    error: error.is_some(),
                    ..FieldLook::default()
                };
                let response = widgets::text_input(
                    ui,
                    p,
                    Id::unique("bt-host"),
                    &mut self.form.connect_host,
                    label,
                    t!("form.connectPlaceholder"),
                    look,
                );
                if response.changed() {
                    self.form.touched.address = true;
                }
            },
        );
        ui.add_space(SPACE_3);
        self.row(
            ui,
            |app, ui| {
                let error = app.error_text(ErrorField::Port);
                field(ui, p, t!("form.port"), error.as_deref(), |ui, label| {
                    let look = FieldLook {
                        mono: true,
                        error: error.is_some(),
                        ..FieldLook::default()
                    };
                    let id = Id::unique("bt-cport");
                    let response = widgets::text_input(
                        ui,
                        p,
                        id,
                        &mut app.form.connect_port_text,
                        label,
                        "",
                        look,
                    );
                    if response.changed() {
                        app.form.touched.port = true;
                        if let Ok(port) = check_port(&app.form.connect_port_text) {
                            app.settings_mut().config.connect_port = port;
                        }
                    }
                });
            },
            |_, _| {},
        );
        ui.add_space(SPACE_3);
        self.responder_field(ui, p);
    }

    /// (в) Кабель COM-порта: порт с «Обновить», скорость, формат, автоответ.
    fn serial_fields(&mut self, ui: &mut Ui, p: &Palette) {
        let error = self.error_text(ErrorField::Serial);
        let ctx = ui.ctx().clone();
        field(ui, p, t!("form.serialPort"), error.as_deref(), |ui, _| {
            let mut ports = self.ports.ready().cloned().unwrap_or_default();
            let saved = self.settings.config.serial.port.clone();
            // Пропавший порт не исчезает из списка сам: он сохранён в настройках.
            if !saved.is_empty() && !ports.iter().any(|port| port.name == saved) {
                ports.push(bytetap_core::transport::serial::PortInfo {
                    name: saved.clone(),
                    description: saved.clone(),
                });
            }
            let mut options = vec![t!("form.serialChoose").to_owned()];
            options.extend(
                ports
                    .iter()
                    .map(|port| format!("{} ({})", port.description, port.name)),
            );
            let current = if saved.is_empty() {
                0
            } else {
                ports
                    .iter()
                    .position(|port| port.name == saved)
                    .map_or(0, |index| index + 1)
            };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = SPACE_2;
                let width = ui.available_width() - 28.0 - SPACE_2;
                ui.allocate_ui(vec2(width, widgets::FIELD_HEIGHT), |ui| {
                    let look = FieldLook {
                        error: error.is_some(),
                        ..FieldLook::default()
                    };
                    let (_, chosen) = widgets::select(
                        ui,
                        p,
                        Id::unique("bt-serial"),
                        t!("form.serialPort"),
                        &options,
                        Some(current),
                        look,
                    );
                    if let Some(index) = chosen {
                        self.form.touched.serial = true;
                        self.settings_mut().config.serial.port = index
                            .checked_sub(1)
                            .and_then(|index| ports.get(index))
                            .map(|port| port.name.clone())
                            .unwrap_or_default();
                    }
                });
                if widgets::icon_button(ui, p, Icon::RefreshCw, 15.0, t!("form.refreshPorts"))
                    .clicked()
                {
                    self.refresh_ports(&ctx);
                }
            });
        });
        ui.add_space(SPACE_3);
        self.row(
            ui,
            |app, ui| {
                field(ui, p, t!("form.baud"), None, |ui, label| {
                    if app.form.baud_other {
                        let response = widgets::text_input(
                            ui,
                            p,
                            Id::unique("bt-baud-other"),
                            &mut app.form.baud_text,
                            label,
                            "",
                            FieldLook {
                                mono: true,
                                ..FieldLook::default()
                            },
                        );
                        if response.changed() {
                            app.form.touched.serial = true;
                            if let Some(baud) = check_baud(&app.form.baud_text) {
                                app.settings_mut().config.serial.baud = baud;
                            }
                        }
                        return;
                    }
                    let mut options: Vec<String> =
                        BAUD_RATES.iter().map(ToString::to_string).collect();
                    options.push(t!("form.baudOther").to_owned());
                    let current = BAUD_RATES
                        .iter()
                        .position(|rate| *rate == app.settings.config.serial.baud);
                    let (_, chosen) = widgets::select(
                        ui,
                        p,
                        Id::unique("bt-baud"),
                        t!("form.baud"),
                        &options,
                        current,
                        FieldLook {
                            mono: true,
                            ..FieldLook::default()
                        },
                    );
                    if let Some(index) = chosen {
                        app.form.touched.serial = true;
                        match BAUD_RATES.get(index) {
                            Some(rate) => {
                                app.settings_mut().config.serial.baud = *rate;
                                app.form.baud_text = rate.to_string();
                            }
                            None => app.form.baud_other = true,
                        }
                    }
                });
            },
            |app, ui| {
                field(ui, p, t!("form.format"), None, |ui, _| {
                    let options: Vec<String> = FORMATS.iter().map(|f| (*f).to_owned()).collect();
                    let format = serial_format(&app.settings.config.serial);
                    let current = FORMATS.iter().position(|f| *f == format);
                    let (_, chosen) = widgets::select(
                        ui,
                        p,
                        Id::unique("bt-format"),
                        t!("form.format"),
                        &options,
                        current,
                        FieldLook {
                            mono: true,
                            ..FieldLook::default()
                        },
                    );
                    if let Some(index) = chosen {
                        apply_format(&mut app.settings_mut().config.serial, FORMATS[index]);
                    }
                });
            },
        );
        ui.add_space(SPACE_3);
        self.responder_field(ui, p);
    }

    /// «Дополнительно»: свёрнуто, пока его не раскрыли или под ним нет ошибки.
    fn more(&mut self, ui: &mut Ui, p: &Palette, mode: SessionMode) {
        ui.add_space(SPACE_4);
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(
            line.x_range(),
            line.center().y,
            Stroke::new(1.0, p.border_faint),
        );
        ui.add_space(SPACE_3);
        let icon = if self.form.more {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let toggle = widgets::link_button(ui, p, icon, 14.0, t!("form.more"), true);
        let expanded = self.form.more;
        toggle.widget_info(|| WidgetInfo::selected(Role::Button, true, expanded, t!("form.more")));
        if toggle.clicked() {
            self.form.more = !self.form.more;
        }
        if !self.form.more {
            return;
        }
        ui.add_space(SPACE_3);
        match mode {
            SessionMode::TcpServer => {
                self.interface_field(ui, p);
                ui.add_space(SPACE_3);
            }
            SessionMode::TcpClient => {
                self.row(
                    ui,
                    |app, ui| {
                        field(ui, p, t!("form.reconnect"), None, |ui, label| {
                            let response = widgets::text_input(
                                ui,
                                p,
                                Id::unique("bt-reconnect"),
                                &mut app.form.reconnect_text,
                                label,
                                "",
                                FieldLook {
                                    mono: true,
                                    ..FieldLook::default()
                                },
                            );
                            if response.changed() {
                                let sec = parse_reconnect(&app.form.reconnect_text);
                                app.settings_mut().config.reconnect_sec = sec;
                            }
                        });
                    },
                    |_, _| {},
                );
                ui.add_space(SPACE_3);
            }
            SessionMode::Serial => {
                self.row(
                    ui,
                    |app, ui| {
                        field(ui, p, t!("form.flow"), None, |ui, _| {
                            let options: Vec<String> =
                                FLOWS.iter().map(|f| flow_text(*f).to_owned()).collect();
                            let current = FLOWS
                                .iter()
                                .position(|f| *f == app.settings.config.serial.flow);
                            let (_, chosen) = widgets::select(
                                ui,
                                p,
                                Id::unique("bt-flow"),
                                t!("form.flow"),
                                &options,
                                current,
                                FieldLook::default(),
                            );
                            if let Some(index) = chosen {
                                app.settings_mut().config.serial.flow = FLOWS[index];
                            }
                        });
                    },
                    |app, ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let mut dtr = app.settings.config.serial.dtr;
                        if widgets::checkbox(ui, p, &mut dtr, t!("form.dtr")).changed() {
                            app.settings_mut().config.serial.dtr = dtr;
                        }
                        let mut rts = app.settings.config.serial.rts;
                        if widgets::checkbox(ui, p, &mut rts, t!("form.rts")).changed() {
                            app.settings_mut().config.serial.rts = rts;
                        }
                    },
                );
                ui.add_space(SPACE_3);
            }
        }
        self.row(
            ui,
            |app, ui| {
                field(ui, p, t!("form.analyzer"), None, |ui, label| {
                    let mut text = app.settings.config.analyzer.clone();
                    let response = widgets::text_input(
                        ui,
                        p,
                        Id::unique("bt-analyzer"),
                        &mut text,
                        label,
                        t!("form.analyzerPlaceholder"),
                        FieldLook::default(),
                    );
                    if response.changed() {
                        app.settings_mut().config.analyzer = text;
                    }
                });
            },
            |app, ui| {
                field(ui, p, t!("form.note"), None, |ui, label| {
                    let mut text = app.settings.config.note.clone();
                    let response = widgets::text_input(
                        ui,
                        p,
                        Id::unique("bt-note"),
                        &mut text,
                        label,
                        t!("form.notePlaceholder"),
                        FieldLook::default(),
                    );
                    if response.changed() {
                        app.settings_mut().config.note = text;
                    }
                });
            },
        );
        ui.add_space(SPACE_3);
        self.log_dir_field(ui, p);
        let responder = self.settings.config.responder;
        if matches!(responder, Responder::Auto | Responder::Astm) {
            ui.add_space(SPACE_3);
            let mut nak = self.settings.config.astm_nak_on_bad_checksum;
            if widgets::checkbox(ui, p, &mut nak, t!("form.nak")).changed() {
                self.settings_mut().config.astm_nak_on_bad_checksum = nak;
            }
        }
        ui.add_space(SPACE_3);
        self.profile_field(ui, p);
    }

    /// Интерфейс прослушивания TCP-сервера; ошибка адреса — под ним.
    fn interface_field(&mut self, ui: &mut Ui, p: &Palette) {
        let error = self.error_text(ErrorField::Address);
        field(ui, p, t!("form.listenOn"), error.as_deref(), |ui, _| {
            let mut hosts = vec![ALL_INTERFACES.to_owned()];
            let mut options = vec![t!("form.allInterfaces").to_owned()];
            for iface in self.interfaces.ready().into_iter().flatten() {
                hosts.push(iface.addr.clone());
                options.push(format!("{} — {}", iface.addr, iface.name));
            }
            let saved = self.settings.config.listen_host.clone();
            if !hosts.contains(&saved) {
                hosts.push(saved.clone());
                options.push(saved.clone());
            }
            let current = hosts.iter().position(|host| *host == saved);
            let look = FieldLook {
                error: error.is_some(),
                ..FieldLook::default()
            };
            let (_, chosen) = widgets::select(
                ui,
                p,
                Id::unique("bt-iface"),
                t!("form.listenOn"),
                &options,
                current,
                look,
            );
            if let Some(index) = chosen {
                self.settings_mut().config.listen_host = hosts[index].clone();
            }
        });
    }

    /// Папка журналов только для чтения и «Выбрать»; ниже — ошибка или предупреждение.
    fn log_dir_field(&mut self, ui: &mut Ui, p: &Palette) {
        let error = self.error_text(ErrorField::LogDir);
        let label = widgets::field_label(ui, p, t!("form.logDir"));
        ui.add_space(SPACE_1);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SPACE_2;
            let chip = chip_width(ui, t!("form.choose"));
            let width = ui.available_width() - chip - SPACE_2;
            ui.allocate_ui(vec2(width, widgets::FIELD_HEIGHT), |ui| {
                let mut dir = self.settings.config.log_dir.clone();
                widgets::text_input(
                    ui,
                    p,
                    Id::unique("bt-log-dir"),
                    &mut dir,
                    &label,
                    "",
                    FieldLook {
                        mono: true,
                        error: error.is_some(),
                        readonly: true,
                    },
                );
            });
            if widgets::chip_button(ui, p, None, t!("form.choose"), true).clicked() {
                let ctx = ui.ctx().clone();
                self.pick_log_dir(&ctx);
            }
        });
        let dir = self.settings.config.log_dir.trim();
        let warning = self
            .log_dir_warning
            .as_ref()
            .filter(|(for_dir, _)| for_dir == dir)
            .and_then(|(_, kind)| *kind);
        if let Some(error) = error {
            ui.add_space(SPACE_1);
            widgets::field_error(ui, p.danger, &error);
        } else if let Some(kind) = warning {
            ui.add_space(SPACE_1);
            let text = match kind {
                bytetap_core::journal::LogDirWarningKind::Onedrive => {
                    t!("form.logDirWarningOnedrive")
                }
                bytetap_core::journal::LogDirWarningKind::Network => {
                    t!("form.logDirWarningNetwork")
                }
            };
            widgets::field_error(ui, p.warn, text);
        }
    }

    /// Профили: выбор меняет поля, «Сохранить» спрашивает имя, «Удалить» — выбранный.
    fn profile_field(&mut self, ui: &mut Ui, p: &Palette) {
        let label = t!("form.profile");
        widgets::field_label(ui, p, label);
        ui.add_space(SPACE_1);
        let names: Vec<String> = self
            .settings
            .profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect();
        let mut options = vec![t!("form.profileLast").to_owned()];
        options.extend(names.iter().cloned());
        let current = names
            .iter()
            .position(|name| *name == self.form.selected_profile)
            .map_or(0, |index| index + 1);
        let mut chosen = None;
        let mut save = false;
        let mut delete = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SPACE_2;
            let chips = chip_width(ui, t!("form.profileSave"))
                + chip_width(ui, t!("form.profileDelete"))
                + 2.0 * SPACE_2;
            ui.allocate_ui(
                vec2(ui.available_width() - chips, widgets::FIELD_HEIGHT),
                |ui| {
                    chosen = widgets::select(
                        ui,
                        p,
                        Id::unique("bt-profile"),
                        label,
                        &options,
                        Some(current),
                        FieldLook::default(),
                    )
                    .1;
                },
            );
            save = widgets::chip_button(ui, p, None, t!("form.profileSave"), true).clicked();
            let selected = !self.form.selected_profile.is_empty();
            delete =
                widgets::chip_button(ui, p, None, t!("form.profileDelete"), selected).clicked();
        });
        if let Some(index) = chosen {
            match index.checked_sub(1).and_then(|index| names.get(index)) {
                Some(name) => self.apply_profile(name.clone()),
                None => self.form.selected_profile.clear(),
            }
        }
        if save {
            let cfg = &self.settings.config;
            let analyzer = cfg.analyzer.trim();
            self.profile_prompt = Some(if analyzer.is_empty() {
                format!(":{}", cfg.listen_port)
            } else {
                analyzer.to_owned()
            });
        }
        if delete {
            let name = std::mem::take(&mut self.form.selected_profile);
            remove_profile(&mut self.settings_mut().profiles, &name);
            self.save_settings();
        }
    }

    /// Профиль заменяет конфигурацию целиком, вместе с режимом; поля следуют за ней.
    fn apply_profile(&mut self, name: String) {
        let Some(profile) = self
            .settings
            .profiles
            .iter()
            .find(|profile| profile.name == name)
        else {
            return;
        };
        let config = profile.config.clone();
        self.form.sync(&config);
        self.form.selected_profile = name;
        self.screen = Screen::Step(config.mode);
        self.settings_mut().config = config;
        self.start_error = None;
        self.save_settings();
    }

    /// Диалог имени профиля: Enter — сохранить, Escape и клик мимо — отмена.
    pub(crate) fn profile_prompt(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.profile_prompt.take() else {
            return;
        };
        let p = self.pal();
        let mut confirm = false;
        let mut cancel = false;
        let modal = egui::Modal::new(Id::unique("bt-profile-prompt"))
            .frame(
                Frame::new()
                    .fill(p.surface_0)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(radius(RADIUS_PANEL))
                    .inner_margin(SPACE_4),
            )
            .backdrop_color(p.text.gamma_multiply(0.35))
            .show(ctx, |ui| {
                ui.set_width(388.0);
                ui.spacing_mut().item_spacing.y = SPACE_3;
                ui.label(
                    RichText::new(t!("actions.profileName"))
                        .font(theme::sans_semibold(TEXT_BASE))
                        .color(p.text),
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = SPACE_1;
                    let label = widgets::field_label(ui, &p, t!("actions.profileName"));
                    let id = Id::unique("bt-profile-name");
                    let response = widgets::text_input(
                        ui,
                        &p,
                        id,
                        &mut name,
                        &label,
                        t!("actions.profileNamePlaceholder"),
                        FieldLook::default(),
                    );
                    if ui.memory(|m| m.focused().is_none()) {
                        response.request_focus();
                    }
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        confirm = true;
                    }
                });
                let filled = !name.trim().is_empty();
                // «Сохранить», «Отмена» — справа, в порядке Windows; кнопки добавляются
                // слева направо, чтобы Tab шёл в том же порядке, что видит глаз.
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SPACE_2;
                    let buttons = widgets::confirm_button_width(ui, t!("actions.save"))
                        + SPACE_2
                        + widgets::chip_button_width(ui, false, t!("actions.cancel"));
                    ui.add_space((ui.available_width() - buttons - SPACE_2).max(0.0));
                    confirm |=
                        widgets::confirm_button(ui, &p, t!("actions.save"), filled).clicked();
                    cancel =
                        widgets::chip_button(ui, &p, None, t!("actions.cancel"), true).clicked();
                });
            });
        if modal.should_close() {
            cancel = true;
        }
        let trimmed = name.trim().to_owned();
        if confirm && !trimmed.is_empty() {
            let name = trimmed;
            let form = self.form.clone();
            let settings = self.settings_mut();
            form.commit(&mut settings.config);
            let config = settings.config.clone();
            upsert_profile(&mut settings.profiles, &name, &config);
            diag::info(format!("профиль сохранён: {name}"));
            self.form.selected_profile = name;
            self.save_settings();
        } else if !cancel {
            self.profile_prompt = Some(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_of_a_hidden_field_goes_under_start() {
        use ErrorField::*;
        assert_eq!(shown_field(Port, SessionMode::TcpServer), Port);
        assert_eq!(shown_field(Address, SessionMode::TcpServer), Address);
        assert_eq!(shown_field(Address, SessionMode::TcpClient), Address);
        assert_eq!(shown_field(Serial, SessionMode::Serial), Serial);
        assert_eq!(shown_field(Serial, SessionMode::TcpClient), Start);
        assert_eq!(shown_field(Port, SessionMode::Serial), Start);
        assert_eq!(shown_field(LogDir, SessionMode::Serial), LogDir);
    }
}
