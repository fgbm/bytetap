//! Шаг сценария — форма одного из трёх режимов (FR-1, FR-2, FR-9, FR-11, FR-14,
//! U1-AC10…AC12), как `ScenarioForm.vue`.
//!
//! Поля минимальные: подпись 12px над полем, само поле 34px с тонкой рамкой; служебное
//! (интерфейс, переподключение, поток, DTR/RTS, анализатор, комментарий, папка журналов,
//! NAK, профили) — под свёрнутым «Дополнительно». Ошибка показывается красным текстом
//! 12px под своим полем, без баннеров.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CursorIcon, Frame, Id, Key, Layout, Modifiers, Rect, RichText, Role,
    Sense, Stroke, Ui, UiBuilder, Vec2, WidgetInfo, vec2,
};

use crate::app::{BytetapApp, Screen, centered};
use crate::background::{self, Loaded, Task};
use crate::errors::ErrorField;
use crate::form::{
    self, FORMATS, apply_format, check_baud, check_port, parse_reconnect, serial_format,
};
use crate::icons::{self, Icon};
use crate::scenarios::mode_title;
use crate::settings::{
    ALL_INTERFACES, BAUD_RATES, Flow, ProxyMode, Responder, SessionMode, remove_profile,
    upsert_profile,
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
/// Пауза без изменений в «Адресе цели», после которой считается адрес источника (FR-8).
///
/// Проба идёт в сеть своим потоком: без паузы каждое нажатие клавиши запускало бы
/// новый поток, а его результат устаревал бы, не дойдя до строки.
const PROBE_DEBOUNCE: Duration = Duration::from_millis(400);

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
/// тогда ошибка встаёт под «Старт». Адрес у сервера — интерфейс прослушивания, у прокси —
/// адрес второй стороны.
pub fn shown_field(field: ErrorField, mode: SessionMode) -> ErrorField {
    // Поля двух портов есть только у прослушки: в остальных режимах ошибка встаёт под
    // «Старт» (M5-AC3).
    if mode != SessionMode::ComMonitor && matches!(field, ErrorField::SerialA | ErrorField::SerialB)
    {
        return ErrorField::Start;
    }
    match (field, mode) {
        (ErrorField::Port | ErrorField::Address | ErrorField::Interface, SessionMode::Serial)
        | (ErrorField::Serial | ErrorField::Interface, SessionMode::TcpClient) => ErrorField::Start,
        // У сервера единственное поле адреса — интерфейс прослушивания.
        (ErrorField::Address, SessionMode::TcpServer) => ErrorField::Interface,
        // У прослушки один порт ошибки не бывает: там пара портов, своё поле у каждой.
        (
            ErrorField::Serial,
            SessionMode::TcpServer | SessionMode::Proxy | SessionMode::ComMonitor,
        ) => ErrorField::Start,
        (
            ErrorField::TargetPort,
            SessionMode::TcpServer | SessionMode::TcpClient | SessionMode::Serial,
        ) => ErrorField::Start,
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

/// Строка-подсказка с иконкой `info` и пояснением при наведении: спокойное
/// предупреждение, а не баннер (FR-8, M4-AC7).
fn hint_with_info(ui: &mut Ui, p: &Palette, text: &str, hint: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        widgets::hint(ui, p, text);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Image, true, hint));
        icons::paint(ui, Icon::Info, rect.center(), 13.0, p.text_faint);
        response
            .on_hover_cursor(CursorIcon::Help)
            .on_hover_text(hint);
    });
}

/// Ширина кнопки-пилюли с подписью: место под неё справа от поля.
fn chip_width(ui: &Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), theme::sans(TEXT_XS), Color32::WHITE)
        .size()
        .x
        + 20.0
}

/// Адрес строки FR-8 по топологии (M4-AC7).
///
/// «Анализатор подключается к ЛИС»: ЛИС определяет анализатор по адресу, с которого
/// bytetap выходит к ней, — его и вписывают в ЛИС. Без пробы и записи остаётся IP
/// прослушивания: порт исходящего соединения случаен и для ЛИС ничего не значит, а порт
/// прослушивания — не тот, с которого bytetap приходит к ней.
///
/// «ЛИС подключается к анализатору»: ЛИС сама идёт на порт прослушивания, и в ней должен
/// стоять адрес прослушивания вместе с портом (он же `open.local` в записи), а не
/// источник исходящего соединения к анализатору.
fn fr8_address(
    role: ProxyMode,
    listen_host: &str,
    listen_addr: &str,
    source: Option<String>,
) -> String {
    match role {
        ProxyMode::AnalyzerClient => source.unwrap_or_else(|| listen_host.to_owned()),
        ProxyMode::AnalyzerServer => listen_addr.to_owned(),
    }
}

/// Прокрутка к виджету, получившему фокус: Tab уводит к полям ниже края окна, а
/// раскрытое «Дополнительно» шага прокси в окно 800 точек не помещается (M4-AC13).
///
/// Поля рисуются в общей прокрутке экрана, поэтому прокручивается не своё поле, а
/// ближайшая к нему прокрутка: виджет целиком попадает в видимую область.
fn scroll_to_focus(ui: &Ui) {
    if let Some(focused) = ui.memory(|memory| memory.focused())
        && let Some(response) = ui.ctx().read_response(focused)
        && response.gained_focus()
    {
        ui.scroll_to_rect(response.rect, None);
    }
}

/// Сторона прослушки: у порта анализатора и у порта ЛИС свои поля и свои ошибки
/// (M5-AC3, M5-В2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MonitorPort {
    Analyzer,
    Lis,
}

impl MonitorPort {
    /// Поле ошибки старта этой стороны.
    fn error(self) -> ErrorField {
        match self {
            Self::Analyzer => ErrorField::SerialA,
            Self::Lis => ErrorField::SerialB,
        }
    }

    /// Подпись поля: «Порт анализатора» или «Порт ЛИС».
    fn label(self) -> &'static str {
        match self {
            Self::Analyzer => t!("form.monitorAnalyzerPort"),
            Self::Lis => t!("form.monitorLisPort"),
        }
    }

    /// Идентификаторы полей: у двух списков портов они разные.
    fn id(self) -> &'static str {
        match self {
            Self::Analyzer => "bt-monitor-port-a",
            Self::Lis => "bt-monitor-port-b",
        }
    }

    fn baud_id(self) -> &'static str {
        match self {
            Self::Analyzer => "bt-monitor-baud-a",
            Self::Lis => "bt-monitor-baud-b",
        }
    }

    fn format_id(self) -> &'static str {
        match self {
            Self::Analyzer => "bt-monitor-format-a",
            Self::Lis => "bt-monitor-format-b",
        }
    }

    /// Настройки порта этой стороны.
    fn serial(self, cfg: &crate::settings::AppConfig) -> &crate::settings::SerialSettings {
        match self {
            Self::Analyzer => &cfg.serial,
            Self::Lis => &cfg.monitor_lis,
        }
    }

    /// Имя выбранного порта этой стороны.
    fn port(self, cfg: &crate::settings::AppConfig) -> &str {
        &self.serial(cfg).port
    }

    fn baud(self, cfg: &crate::settings::AppConfig) -> u32 {
        self.serial(cfg).baud
    }

    fn baud_other(self, form: &crate::form::FormState) -> bool {
        match self {
            Self::Analyzer => form.baud_other,
            Self::Lis => form.lis_baud_other,
        }
    }

    /// Текст «другой скорости» этой стороны.
    fn baud_text(self, form: &mut crate::form::FormState) -> &mut String {
        match self {
            Self::Analyzer => &mut form.baud_text,
            Self::Lis => &mut form.lis_baud_text,
        }
    }
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
        // Адреса этого компьютера: по ним форма проверяет, что цель прокси не ведёт на
        // собственный порт bytetap.
        self.form.own_addresses = self
            .interfaces
            .ready()
            .map(|list| list.iter().map(|iface| iface.addr.clone()).collect())
            .unwrap_or_default();
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
                SessionMode::Proxy => self.proxy_fields(ui, &p),
                SessionMode::ComMonitor => self.monitor_fields(ui, &p),
            }
            self.more(ui, &p, mode);
            ui.add_space(SPACE_4);
            let idle = self.phase != crate::live::Phase::Starting;
            start = widgets::start_button(ui, &p, t!("form.start"), idle).clicked() && idle;
            if let Some(error) = self.error_text(ErrorField::Start) {
                ui.add_space(SPACE_2);
                ui.vertical_centered(|ui| widgets::field_error(ui, p.danger, &error));
            }
            ui.add_space(SPACE_5);
            scroll_to_focus(ui);
        });
        if back {
            self.back_to_scenarios(&ctx);
        } else if start {
            self.start(&ctx);
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
            |app, ui| app.listen_port_field(ui, p, t!("form.port")),
            |_, _| {},
        );
        ui.add_space(SPACE_3);
        self.responder_field(ui, p);
        ui.add_space(SPACE_3 + SPACE_4);
        self.address_block(ui, p, None);
    }

    /// Порт, который слушает bytetap: одно поле у сервера и у прокси, подпись — своя.
    fn listen_port_field(&mut self, ui: &mut Ui, p: &Palette, label: &str) {
        let error = self.error_text(ErrorField::Port);
        field(ui, p, label, error.as_deref(), |ui, label| {
            let look = FieldLook {
                mono: true,
                error: error.is_some(),
                ..FieldLook::default()
            };
            let id = Id::unique("bt-port");
            let response =
                widgets::text_input(ui, p, id, &mut self.form.port_text, label, "", look);
            if response.changed() {
                self.form.touched.port = true;
                if let Ok(port) = check_port(&self.form.port_text) {
                    self.settings_mut().config.listen_port = port;
                }
            }
        });
    }

    /// (г) Встать между анализатором и ЛИС (FR-5…FR-8): топология, порт прослушивания с
    /// интерфейсом одной строкой, цель, крупный адрес для второй стороны.
    fn proxy_fields(&mut self, ui: &mut Ui, p: &Palette) {
        let role = self.settings.config.proxy_role;
        widgets::field_label(ui, p, t!("form.topology"));
        ui.add_space(SPACE_1);
        let client_id = Id::unique("bt-proxy-client");
        let server_id = Id::unique("bt-proxy-server");
        // Стрелка переключила пару в прошлом кадре: фокус возвращается на выбранную
        // кнопку, а не уезжает по навигации egui к следующему полю.
        if let Some(target) = self.form.radio_focus.take() {
            let id = match target {
                ProxyMode::AnalyzerClient => client_id,
                ProxyMode::AnalyzerServer => server_id,
            };
            ui.memory_mut(|memory| memory.request_focus(id));
        }
        // Пара — один узел AccessKit с ролью `RadioGroup` и подписью «Топология»:
        // экранный диктор читает её как группу выбора, а не как две кнопки подряд.
        let mut client = None;
        let mut server = None;
        ui.scope_builder(
            UiBuilder::new()
                .accessibility_role(Role::RadioGroup)
                .accessibility_label(t!("form.topology")),
            |ui| {
                client = Some(widgets::radio(
                    ui,
                    p,
                    client_id,
                    role == ProxyMode::AnalyzerClient,
                    t!("form.topologyAnalyzerClient"),
                ));
                ui.add_space(2.0);
                server = Some(widgets::radio(
                    ui,
                    p,
                    server_id,
                    role == ProxyMode::AnalyzerServer,
                    t!("form.topologyAnalyzerServer"),
                ));
            },
        );
        let (client, server) = (client.expect("first radio"), server.expect("second radio"));
        // Радио-пара — один выбор: стрелки ходят по ней, фокус переходит выбранной
        // кнопке (M4-В1). Tab обходит обе, как обычные поля формы, Space нажимает ту,
        // что в фокусе.
        let mut chosen = None;
        if client.clicked() || client.changed() {
            chosen = Some(ProxyMode::AnalyzerClient);
        }
        if server.clicked() || server.changed() {
            chosen = Some(ProxyMode::AnalyzerServer);
        }
        let focused = if client.has_focus() {
            Some(ProxyMode::AnalyzerClient)
        } else if server.has_focus() {
            Some(ProxyMode::AnalyzerServer)
        } else {
            None
        };
        if let Some(current) = focused {
            let (previous, next) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp)
                        || i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown)
                        || i.consume_key(Modifiers::NONE, Key::ArrowRight),
                )
            });
            if previous || next {
                // Кнопок две: и «предыдущая», и «следующая» — вторая из пары.
                let other = match current {
                    ProxyMode::AnalyzerClient => ProxyMode::AnalyzerServer,
                    ProxyMode::AnalyzerServer => ProxyMode::AnalyzerClient,
                };
                chosen = Some(other);
                let id = match other {
                    ProxyMode::AnalyzerClient => client_id,
                    ProxyMode::AnalyzerServer => server_id,
                };
                ui.memory_mut(|memory| memory.request_focus(id));
                self.form.radio_focus = Some(other);
            }
        }
        if let Some(role) = chosen {
            self.settings_mut().config.proxy_role = role;
        }

        ui.add_space(SPACE_2);
        self.row(
            ui,
            |app, ui| app.listen_port_field(ui, p, t!("form.listenPortProxy")),
            |app, ui| {
                let error = app.error_text(ErrorField::Interface);
                app.interface_field(ui, p, error);
            },
        );
        ui.add_space(SPACE_2);
        self.row(
            ui,
            |app, ui| {
                let error = app.error_text(ErrorField::Address);
                let label = match app.settings.config.proxy_role {
                    ProxyMode::AnalyzerClient => t!("form.proxyLisAddress"),
                    ProxyMode::AnalyzerServer => t!("form.proxyAnalyzerAddress"),
                };
                field(ui, p, label, error.as_deref(), |ui, label| {
                    let look = FieldLook {
                        mono: true,
                        error: error.is_some(),
                        ..FieldLook::default()
                    };
                    let response = widgets::text_input(
                        ui,
                        p,
                        Id::unique("bt-proxy-host"),
                        &mut app.form.proxy_host,
                        label,
                        t!("form.connectPlaceholder"),
                        look,
                    );
                    if response.changed() {
                        app.form.touched.address = true;
                    }
                });
            },
            |app, ui| {
                let error = app.error_text(ErrorField::TargetPort);
                field(
                    ui,
                    p,
                    t!("form.targetPort"),
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
                            Id::unique("bt-proxy-port"),
                            &mut app.form.proxy_port_text,
                            label,
                            t!("form.targetPortPlaceholder"),
                            look,
                        );
                        if response.changed() {
                            app.form.touched.target_port = true;
                            if let Ok(port) = check_port(&app.form.proxy_port_text) {
                                app.settings_mut().config.proxy_port = port;
                            }
                        }
                    },
                );
            },
        );
        // Поля автоответа в прокси нет: прибор получает ответы от ЛИС, а не от bytetap.
        ui.add_space(SPACE_2);
        widgets::hint(ui, p, t!("form.noResponderInProxy"));
        ui.add_space(SPACE_3);
        self.address_block(ui, p, Some(role));
    }

    /// Адрес, который вписывается во вторую сторону: адрес ноутбука и порт, крупно.
    ///
    /// `proxy` — топология прокси: над адресом появляется «Впишите в анализатор» (или «в
    /// ЛИС»), под ним — предупреждение FR-8 о том, что вторая сторона определяет
    /// собеседника по IP-адресу. Спокойная muted-строка, без баннера и окна (M4-AC7).
    fn address_block(&mut self, ui: &mut Ui, p: &Palette, proxy: Option<ProxyMode>) {
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
        if let Some(role) = proxy {
            ui.label(
                RichText::new(match role {
                    ProxyMode::AnalyzerClient => t!("form.writeIntoAnalyzer"),
                    ProxyMode::AnalyzerServer => t!("form.writeIntoLis"),
                })
                .size(TEXT_XS)
                .color(p.text_muted),
            );
            ui.add_space(SPACE_1 / 2.0);
        }
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
        if let Some(role) = proxy {
            // FR-8: вторая сторона определяет собеседника по IP, и адрес строки зависит
            // от топологии (M4-AC7): куда смотреть — в `fr8_address`.
            let ctx = ui.ctx().clone();
            let source = self.source_address(&ctx);
            let shown = fr8_address(role, &host, &address, source);
            let text = match role {
                ProxyMode::AnalyzerClient => t!("form.proxyWarning", addr = shown),
                ProxyMode::AnalyzerServer => t!("form.proxyWarningServer", addr = shown),
            };
            hint_with_info(ui, p, &text, t!("form.proxyWarningHint"));
        }
        let firewall_hint = if proxy.is_some() {
            // В прокси приложение не «не ходит в сеть», а соединяется со второй стороной.
            t!("form.firewallHintProxy")
        } else {
            t!("form.firewallHint")
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            widgets::hint(ui, p, t!("form.firewall"));
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::Image, true, firewall_hint));
            icons::paint(ui, Icon::Info, rect.center(), 13.0, p.text_faint);
            response
                .on_hover_cursor(CursorIcon::Help)
                .on_hover_text(firewall_hint);
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

    /// Правка скорости или формата анализатора: второй порт повторяет её, пока его не
    /// трогали (M5-В2).
    fn analyzer_changed(&mut self, change: impl FnOnce(&mut crate::settings::AppConfig)) {
        let mut config = self.settings.config.clone();
        change(&mut config);
        self.form.analyzer_settings_changed(&mut config);
        self.settings_mut().config = config;
    }

    /// (д) Встать в разрыв COM-кабеля (FR-10): два порта, скорость и формат у каждого,
    /// строка о выключенном автоответе и о том, кто занимает порты (M5-AC11).
    fn monitor_fields(&mut self, ui: &mut Ui, p: &Palette) {
        self.monitor_port_field(ui, p, MonitorPort::Analyzer);
        ui.add_space(SPACE_3);
        self.row(
            ui,
            |app, ui| app.monitor_baud_field(ui, p, MonitorPort::Analyzer),
            |app, ui| app.monitor_format_field(ui, p, MonitorPort::Analyzer),
        );
        ui.add_space(SPACE_3);
        self.monitor_port_field(ui, p, MonitorPort::Lis);
        ui.add_space(SPACE_3);
        self.row(
            ui,
            |app, ui| app.monitor_baud_field(ui, p, MonitorPort::Lis),
            |app, ui| app.monitor_format_field(ui, p, MonitorPort::Lis),
        );
        // Автоответчика в прослушке нет: байты идут как есть, ответы приходят от ЛИС
        // (FR-10). Второй строкой — кто держит порты: в разрыве это только bytetap.
        ui.add_space(SPACE_3);
        widgets::hint(ui, p, t!("form.noResponderInProxy"));
        ui.add_space(SPACE_1);
        widgets::hint(ui, p, t!("form.portsLocked"));
        ui.add_space(SPACE_1);
    }

    /// Порт одной стороны прослушки: список с описанием и «Обновить» (FR-12).
    fn monitor_port_field(&mut self, ui: &mut Ui, p: &Palette, side: MonitorPort) {
        let error = self.error_text(side.error());
        let ctx = ui.ctx().clone();
        let label = side.label();
        field(ui, p, label, error.as_deref(), |ui, _| {
            let mut ports = self.ports.ready().cloned().unwrap_or_default();
            let saved = side.port(&self.settings.config).to_owned();
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
                        Id::unique(side.id()),
                        label,
                        &options,
                        Some(current),
                        look,
                    );
                    if let Some(index) = chosen {
                        let name = index
                            .checked_sub(1)
                            .and_then(|index| ports.get(index))
                            .map(|port| port.name.clone())
                            .unwrap_or_default();
                        match side {
                            MonitorPort::Analyzer => {
                                self.form.touched.serial = true;
                                self.settings_mut().config.serial.port = name;
                            }
                            MonitorPort::Lis => {
                                self.form.touched.serial_b = true;
                                self.settings_mut().config.monitor_lis.port = name;
                            }
                        }
                    }
                });
                if widgets::icon_button(ui, p, Icon::RefreshCw, 15.0, t!("form.refreshPorts"))
                    .clicked()
                {
                    self.refresh_ports(&ctx);
                }
            });
        });
    }

    /// Скорость одной стороны прослушки: у второго порта своя (M5-В2).
    fn monitor_baud_field(&mut self, ui: &mut Ui, p: &Palette, side: MonitorPort) {
        field(ui, p, t!("form.baud"), None, |ui, label| {
            let other = side.baud_other(&self.form);
            if other {
                let text = side.baud_text(&mut self.form);
                let response = widgets::text_input(
                    ui,
                    p,
                    Id::unique(side.baud_id()),
                    text,
                    label,
                    "",
                    FieldLook {
                        mono: true,
                        ..FieldLook::default()
                    },
                );
                if response.changed() {
                    let baud = check_baud(side.baud_text(&mut self.form));
                    if let Some(baud) = baud {
                        match side {
                            MonitorPort::Analyzer => {
                                self.analyzer_changed(|cfg| cfg.serial.baud = baud)
                            }
                            MonitorPort::Lis => {
                                self.form.touched.serial_b = true;
                                self.form.lis_follows = false;
                                self.settings_mut().config.monitor_lis.baud = baud;
                            }
                        }
                    }
                }
                return;
            }
            let mut options: Vec<String> = BAUD_RATES.iter().map(ToString::to_string).collect();
            options.push(t!("form.baudOther").to_owned());
            let current = BAUD_RATES
                .iter()
                .position(|rate| *rate == side.baud(&self.settings.config));
            let (_, chosen) = widgets::select(
                ui,
                p,
                Id::unique(side.baud_id()),
                t!("form.baud"),
                &options,
                current,
                FieldLook {
                    mono: true,
                    ..FieldLook::default()
                },
            );
            if let Some(index) = chosen {
                match BAUD_RATES.get(index) {
                    Some(rate) => match side {
                        MonitorPort::Analyzer => {
                            self.form.touched.serial = true;
                            self.analyzer_changed(|cfg| cfg.serial.baud = *rate);
                        }
                        MonitorPort::Lis => {
                            self.form.touched.serial_b = true;
                            self.form.lis_follows = false;
                            self.settings_mut().config.monitor_lis.baud = *rate;
                        }
                    },
                    None => match side {
                        MonitorPort::Analyzer => self.form.baud_other = true,
                        MonitorPort::Lis => self.form.lis_baud_other = true,
                    },
                }
            }
        });
    }

    /// Формат одной стороны прослушки: `8N1`, `7E2` и прочие из списка (FR-11).
    fn monitor_format_field(&mut self, ui: &mut Ui, p: &Palette, side: MonitorPort) {
        field(ui, p, t!("form.format"), None, |ui, _| {
            let options: Vec<String> = FORMATS.iter().map(|f| (*f).to_owned()).collect();
            let format = serial_format(side.serial(&self.settings.config));
            let current = FORMATS.iter().position(|f| *f == format);
            let (_, chosen) = widgets::select(
                ui,
                p,
                Id::unique(side.format_id()),
                t!("form.format"),
                &options,
                current,
                FieldLook {
                    mono: true,
                    ..FieldLook::default()
                },
            );
            if let Some(index) = chosen {
                match side {
                    MonitorPort::Analyzer => {
                        self.analyzer_changed(|cfg| apply_format(&mut cfg.serial, FORMATS[index]));
                    }
                    MonitorPort::Lis => {
                        self.form.touched.serial_b = true;
                        self.form.lis_follows = false;
                        apply_format(&mut self.settings_mut().config.monitor_lis, FORMATS[index]);
                    }
                }
            }
        });
    }

    /// «Поток и линии» прослушки: поток и два флажка одной строкой, под ней —
    /// предупреждение о том, что насквозь они не идут (M5-В2, М5-В3).
    fn monitor_flow_lines(&mut self, ui: &mut Ui, p: &Palette) {
        widgets::field_label(ui, p, t!("form.flowLines"));
        ui.add_space(SPACE_1);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SPACE_2;
            let width = ui.available_width() / 2.0;
            ui.allocate_ui(vec2(width, widgets::FIELD_HEIGHT), |ui| {
                let options: Vec<String> = FLOWS.iter().map(|f| flow_text(*f).to_owned()).collect();
                let current = FLOWS
                    .iter()
                    .position(|f| *f == self.settings.config.serial.flow);
                let (_, chosen) = widgets::select(
                    ui,
                    p,
                    Id::unique("bt-monitor-flow"),
                    t!("form.flow"),
                    &options,
                    current,
                    FieldLook::default(),
                );
                if let Some(index) = chosen {
                    let flow = FLOWS[index];
                    self.settings_mut().config.serial.flow = flow;
                    self.settings_mut().config.monitor_lis.flow = flow;
                }
            });
            let mut dtr = self.settings.config.serial.dtr;
            if widgets::checkbox(ui, p, &mut dtr, t!("form.dtrOn")).changed() {
                self.settings_mut().config.serial.dtr = dtr;
                self.settings_mut().config.monitor_lis.dtr = dtr;
            }
            let mut rts = self.settings.config.serial.rts;
            if widgets::checkbox(ui, p, &mut rts, t!("form.rtsOn")).changed() {
                self.settings_mut().config.serial.rts = rts;
                self.settings_mut().config.monitor_lis.rts = rts;
            }
        });
        // Линии управления не передаются сквозь bytetap: CTS, DSR и DCD каждой стороны —
        // её собственные линии, а XON/XOFF драйвер съедает из обмена (М5-В3).
        let warning = match self.settings.config.serial.flow {
            Flow::None => None,
            Flow::Rtscts => Some(t!("form.flowNotForwarded")),
            Flow::Xonxoff => Some(t!("form.flowNotForwardedXonxoff")),
        };
        if let Some(text) = warning {
            ui.add_space(SPACE_1);
            widgets::field_error(ui, p.warn, text);
        }
    }

    /// Адрес, с которого bytetap выходит к цели (FR-8).
    ///
    /// Сначала — фактический адрес из первого `open` записи (`link.local` исходящего
    /// сокета к цели): точнее него не бывает, и он помнится вместе с целью, к которой
    /// записан, — к другой цели он не относится. Затем — посчитанный в фоне по цели,
    /// набранной в форме (а не принятой в настройки: цель могли сменить и не нажать
    /// «Старт»): соединение UDP не отправляет пакетов, но выбирает локальный адрес по
    /// таблице маршрутов. `None` — адрес неизвестен.
    ///
    /// В топологии «ЛИС подключается к анализатору» проба не нужна: ЛИС идёт на порт
    /// прослушивания, а исходящее соединение к анализатору в строке FR-8 не участвует.
    fn source_address(&mut self, ctx: &egui::Context) -> Option<String> {
        if self.settings.config.proxy_role == ProxyMode::AnalyzerServer {
            return None;
        }
        let host = self.form.proxy_host.trim().to_owned();
        let port = check_port(&self.form.proxy_port_text).unwrap_or(0);
        let key = form::target_key(&host, port)?;
        if let Some((for_target, addr)) = &self.proxy_source {
            if *for_target == key {
                return Some(addr.clone());
            }
            // Цель сменилась: адрес прошлой записи к новой не относится.
            self.proxy_source = None;
        }
        // Снимки и тесты: окружение синтетическое, в сеть не ходим.
        if self.fixtures {
            return None;
        }
        // Цель, на которую ругается поле, не спрашиваем: адреса некуда показать, а поток
        // на каждое нажатие клавиши — лишняя работа (FR-8).
        if form::target_host_problem(&host).is_some() {
            return None;
        }
        // Дебаунс: пока в «Адресе цели» набирают, проба не начинается. Ключ помнится
        // вместе с моментом последнего изменения, а кадр запрашивается к концу паузы:
        // иначе окно без движения мыши не перерисовалось бы (FR-8).
        match &self.proxy_probe_pending {
            Some((pending, since)) if *pending == key => {
                let left = PROBE_DEBOUNCE.saturating_sub(since.elapsed());
                if !left.is_zero() {
                    ctx.request_repaint_after(left);
                    return None;
                }
            }
            _ => {
                self.proxy_probe_pending = Some((key, Instant::now()));
                ctx.request_repaint_after(PROBE_DEBOUNCE);
                return None;
            }
        }
        // Одна проба в полёте: чужую не бросаем, а её результат к этой цели не относится
        // и отбрасывается по ключу.
        if let Some((probed, probe)) = &self.proxy_probe {
            if *probed == key {
                return probe.ready().cloned().flatten();
            }
            if matches!(probe, Loaded::Loading(_)) {
                ctx.request_repaint_after(PROBE_DEBOUNCE);
                return None;
            }
        }
        let probe_host = form::unbracketed(&host).to_owned();
        let hook = self.proxy_probe_hook.clone();
        self.proxy_probe = Some((
            key,
            Loaded::Loading(Task::spawn(ctx, move || match hook {
                Some(probe) => probe(&probe_host, port),
                None => background::source_addr(&probe_host, port),
            })),
        ));
        None
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
                let error = self.error_text(ErrorField::Interface);
                self.interface_field(ui, p, error);
                ui.add_space(SPACE_3);
            }
            // В прокси интерфейс стоит рядом с портом: в «Дополнительно» его нет,
            // зато здесь фильтр по адресу входящего (FR-8).
            SessionMode::Proxy => {
                self.allow_from_field(ui, p);
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
            // Прослушка: поток и линии общие для обоих портов (M5-В2), под строкой —
            // предупреждение о том, что насквозь они не идут (М5-В3).
            SessionMode::ComMonitor => {
                self.monitor_flow_lines(ui, p);
                ui.add_space(SPACE_3);
                self.row(
                    ui,
                    |app, ui| {
                        field(ui, p, t!("form.reconnect"), None, |ui, label| {
                            let response = widgets::text_input(
                                ui,
                                p,
                                Id::unique("bt-monitor-reconnect"),
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
        // В прокси автоответчика нет вовсе: «NAK на неверную сумму ASTM» к нему не
        // относится, поля в режиме нет (FR-6, M4-В6).
        if !matches!(mode, SessionMode::Proxy | SessionMode::ComMonitor)
            && matches!(responder, Responder::Auto | Responder::Astm)
        {
            ui.add_space(SPACE_3);
            let mut nak = self.settings.config.astm_nak_on_bad_checksum;
            if widgets::checkbox(ui, p, &mut nak, t!("form.nak")).changed() {
                self.settings_mut().config.astm_nak_on_bad_checksum = nak;
            }
        }
        ui.add_space(SPACE_3);
        self.profile_field(ui, p);
    }

    /// Интерфейс прослушивания; `error` — ошибка адреса, если поле её показывает
    /// (у TCP-сервера — в «Дополнительно», у прокси — рядом с портом).
    fn interface_field(&mut self, ui: &mut Ui, p: &Palette, error: Option<String>) {
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

    /// Фильтр по адресу входящего: пусто — принимать с любого (FR-8).
    ///
    /// Без фильтра любой узел сети, достучавшийся до порта, говорит с ЛИС от имени
    /// анализатора, поэтому поле стоит в «Дополнительно» и подсказкой называет, чей
    /// адрес вписывать в каждой топологии.
    fn allow_from_field(&mut self, ui: &mut Ui, p: &Palette) {
        let error = self.error_text(ErrorField::AllowFrom);
        field(
            ui,
            p,
            t!("form.allowFrom"),
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
                    Id::unique("bt-allow-from"),
                    &mut self.form.proxy_allow_from,
                    label,
                    t!("form.allowFromPlaceholder"),
                    look,
                );
                if response.changed() {
                    self.form.touched.allow_from = true;
                    self.settings_mut().config.proxy_allow_from =
                        self.form.proxy_allow_from.trim().to_owned();
                }
            },
        );
        if error.is_none() {
            widgets::hint(ui, p, t!("form.allowFromHint"));
        }
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

    /// Адрес строки FR-8 выбирается по топологии (M4-AC7): «анализатор — клиент» —
    /// источник исходящего соединения к ЛИС (без него — адрес прослушивания), «ЛИС
    /// подключается к анализатору» — всегда адрес прослушивания, к которому идёт ЛИС.
    #[test]
    fn the_fr8_address_depends_on_the_topology() {
        assert_eq!(
            fr8_address(
                ProxyMode::AnalyzerClient,
                "192.168.1.10",
                "192.168.1.10:50025",
                Some("192.168.1.77".to_owned())
            ),
            "192.168.1.77",
            "в ЛИС вписывают адрес, с которого bytetap выходит к ней"
        );
        assert_eq!(
            fr8_address(
                ProxyMode::AnalyzerClient,
                "192.168.1.10",
                "192.168.1.10:50025",
                None
            ),
            "192.168.1.10",
            "без пробы и записи остаётся IP прослушивания, но не порт"
        );
        assert_eq!(
            fr8_address(
                ProxyMode::AnalyzerServer,
                "192.168.1.10",
                "192.168.1.10:5000",
                Some("192.168.1.77".to_owned())
            ),
            "192.168.1.10:5000",
            "ЛИС подключается к адресу прослушивания, а не к источнику соединения"
        );
    }

    /// Проба FR-8 спрашивает набранную в форме цель, а не принятую в настройки, и адрес
    /// прошлой записи помнится с ключом цели (M4-AC7).
    #[test]
    fn the_fr8_probe_asks_about_the_typed_target() {
        use std::sync::{Arc, Mutex};

        let ctx = egui::Context::default();
        let mut settings = crate::settings::Settings::with_log_dir("C:/logs");
        settings.config.mode = SessionMode::Proxy;
        settings.config.proxy_role = ProxyMode::AnalyzerClient;
        // Настройки говорят одно, форма — другое: цель набрали и «Старт» не нажимали.
        settings.config.proxy_host = "10.0.0.9".into();
        settings.config.proxy_port = 5001;
        let mut app = BytetapApp::new(&ctx, settings, None, None);
        app.form.proxy_host = "10.0.0.5".into();
        app.form.proxy_port_text = "5000".into();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&asked);
        app.proxy_probe_hook = Some(Arc::new(move |host: &str, port: u16| {
            seen.lock().expect("список").push(format!("{host}:{port}"));
            Some("192.168.1.10".to_owned())
        }));

        assert_eq!(app.source_address(&ctx), None, "проба идёт в фоне");
        let probed = wait_for_probe(&mut app, &ctx);
        assert_eq!(probed.as_deref(), Some("192.168.1.10"));
        assert_eq!(
            asked.lock().expect("список").as_slice(),
            ["10.0.0.5:5000"],
            "проба спросила набранную цель"
        );

        // Цель сменили: адрес прошлой записи к ней не относится, проба идёт заново.
        app.proxy_source = Some(("10.0.0.5:5000".to_owned(), "192.168.1.99".to_owned()));
        app.form.proxy_host = "10.0.0.6".into();
        assert_eq!(
            app.source_address(&ctx),
            None,
            "адрес другой цели не показывается"
        );
        assert!(app.proxy_source.is_none(), "адрес прошлой цели забыт");
        wait_for_probe(&mut app, &ctx);
        assert_eq!(
            asked.lock().expect("список").last().map(String::as_str),
            Some("10.0.0.6:5000"),
            "проба спросила новую цель"
        );

        // Та же цель, что в записи: её адрес точнее пробы.
        app.proxy_source = Some(("10.0.0.6:5000".to_owned(), "192.168.1.98".to_owned()));
        assert_eq!(
            app.source_address(&ctx).as_deref(),
            Some("192.168.1.98"),
            "адрес из записи относится к этой цели"
        );
    }

    /// Проба FR-8 ждёт, пока цель перестанут менять, и держит один поток: набранное по
    /// буквам имя спрашивается у сети один раз, а цель с ошибкой у поля — ни разу
    /// (M4-AC18).
    #[test]
    fn the_fr8_probe_waits_for_the_target_to_settle() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let ctx = egui::Context::default();
        let mut settings = crate::settings::Settings::with_log_dir("C:/logs");
        settings.config.mode = SessionMode::Proxy;
        settings.config.proxy_role = ProxyMode::AnalyzerClient;
        let mut app = BytetapApp::new(&ctx, settings, None, None);
        app.form.proxy_port_text = "5000".into();
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let counted = Arc::clone(&calls);
        let seen = Arc::clone(&asked);
        app.proxy_probe_hook = Some(Arc::new(move |host: &str, port: u16| {
            counted.fetch_add(1, Ordering::SeqCst);
            seen.lock().expect("список").push(format!("{host}:{port}"));
            Some("192.168.1.10".to_owned())
        }));

        // Цель набирают по буквам: проба не начинается, пока набор не кончился.
        for host in ["1", "10", "10.0", "10.0.0", "10.0.0.5"] {
            app.form.proxy_host = host.into();
            assert_eq!(app.source_address(&ctx), None, "{host}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0, "проба ждёт паузы");

        // Пауза прошла: одна проба по последней цели, а не по каждой букве.
        let probed = wait_for_probe(&mut app, &ctx);
        assert_eq!(probed.as_deref(), Some("192.168.1.10"));
        assert_eq!(
            asked.lock().expect("список").as_slice(),
            ["10.0.0.5:5000"],
            "спрошена только конечная цель"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "один поток на всю набранную цель"
        );

        // Цель, на которую ругается поле, в сеть не идёт вовсе.
        app.form.proxy_host = "10.0.0.5:5000".into();
        for _ in 0..40 {
            assert_eq!(app.source_address(&ctx), None);
            std::thread::sleep(Duration::from_millis(15));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "цель с ошибкой не спрашивается"
        );
    }

    /// Ждёт результат фоновой пробы FR-8: поток отдаёт его через канал.
    fn wait_for_probe(app: &mut BytetapApp, ctx: &egui::Context) -> Option<String> {
        for _ in 0..400 {
            if let Some((_, probe)) = &mut app.proxy_probe {
                probe.poll();
            }
            if let Some(addr) = app.source_address(ctx) {
                return Some(addr);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("проба не ответила");
    }

    #[test]
    fn an_error_of_a_hidden_field_goes_under_start() {
        use ErrorField::*;
        assert_eq!(shown_field(Port, SessionMode::TcpServer), Port);
        assert_eq!(
            shown_field(Address, SessionMode::TcpServer),
            Interface,
            "у сервера единственное поле адреса — интерфейс"
        );
        assert_eq!(shown_field(Interface, SessionMode::TcpServer), Interface);
        assert_eq!(shown_field(Interface, SessionMode::TcpClient), Start);
        assert_eq!(shown_field(Interface, SessionMode::Proxy), Interface);
        assert_eq!(shown_field(Address, SessionMode::TcpClient), Address);
        assert_eq!(shown_field(Serial, SessionMode::Serial), Serial);
        assert_eq!(shown_field(Serial, SessionMode::TcpClient), Start);
        assert_eq!(shown_field(Port, SessionMode::Serial), Start);
        assert_eq!(shown_field(LogDir, SessionMode::Serial), LogDir);
        // Прокси: своё поле у порта прослушивания, адреса цели и порта цели.
        assert_eq!(shown_field(Port, SessionMode::Proxy), Port);
        assert_eq!(shown_field(Address, SessionMode::Proxy), Address);
        assert_eq!(shown_field(TargetPort, SessionMode::Proxy), TargetPort);
        // В чужих режимах поля порта цели нет: ошибка встаёт под «Старт».
        assert_eq!(shown_field(TargetPort, SessionMode::TcpServer), Start);
        assert_eq!(shown_field(Serial, SessionMode::Proxy), Start);
    }
}
