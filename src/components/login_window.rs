use crate::{
    action::Action,
    app_context::AppContext,
    app_error::AppError,
    components::component_traits::{Component, HandleFocus},
    event::Event,
    tg::login_phase::TdAuth,
};
use crossterm::event::{KeyCode, KeyModifiers};
use qrcode::{Color as QrColor, EcLevel, QrCode};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::{io, mem::Discriminant, sync::Arc};
use unicode_width::UnicodeWidthStr;

const TEXT_CARD_WIDTH: u16 = 52;

/// Which name field is active while Telegram asks for registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameField {
    First,
    Last,
}

/// Text the user is editing. TDLib does not know about this.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoginForm {
    Menu {
        selected: usize,
    },
    Phone {
        text: String,
    },
    Code {
        text: String,
    },
    Password {
        text: String,
    },
    Email {
        text: String,
    },
    EmailCode {
        text: String,
    },
    Registration {
        first: String,
        last: String,
        field: NameField,
    },
}

impl Default for LoginForm {
    fn default() -> Self {
        Self::Menu { selected: 0 }
    }
}

/// Sign-in card shown before [`TdAuth::Ready`].
pub struct LoginWindow {
    app_context: Arc<AppContext>,
    form: LoginForm,
    error: Option<String>,
    /// QR was requested and TDLib has not emitted the link yet.
    qr_pending: bool,
    busy: bool,
    seen: Discriminant<TdAuth>,
}

impl LoginWindow {
    pub fn new(app_context: Arc<AppContext>) -> Self {
        Self {
            app_context,
            form: LoginForm::default(),
            error: None,
            qr_pending: false,
            busy: false,
            seen: std::mem::discriminant(&TdAuth::Starting),
        }
    }

    fn sync_form(&mut self, auth: &TdAuth) {
        let kind = std::mem::discriminant(auth);
        if kind == self.seen {
            return;
        }
        self.seen = kind;
        self.error = None;
        self.qr_pending = false;
        self.busy = false;
        self.form = match auth {
            TdAuth::WaitCode => LoginForm::Code {
                text: String::new(),
            },
            TdAuth::WaitPassword => LoginForm::Password {
                text: String::new(),
            },
            TdAuth::WaitEmail => LoginForm::Email {
                text: String::new(),
            },
            TdAuth::WaitEmailCode => LoginForm::EmailCode {
                text: String::new(),
            },
            TdAuth::WaitRegistration => LoginForm::Registration {
                first: String::new(),
                last: String::new(),
                field: NameField::First,
            },
            TdAuth::WaitPhoneNumber => LoginForm::Menu { selected: 0 },
            _ => LoginForm::Menu { selected: 0 },
        };
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        if modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('c') | KeyCode::Char('q'))
        {
            return Some(Action::Quit);
        }

        let auth = self.app_context.td_auth();
        self.sync_form(&auth);

        if matches!(code, KeyCode::Esc | KeyCode::Char('q'))
            && (self.qr_pending
                || matches!(
                    (&auth, &self.form),
                    (TdAuth::WaitOtherDevice { .. }, _)
                        | (TdAuth::Starting, _)
                        | (TdAuth::WaitPhoneNumber, LoginForm::Menu { .. })
                ))
        {
            return Some(Action::Quit);
        }

        if code == KeyCode::Esc && matches!(self.form, LoginForm::Phone { .. }) {
            self.form = LoginForm::Menu { selected: 1 };
            self.error = None;
            self.app_context.mark_dirty();
            return None;
        }

        if self.busy {
            return None;
        }

        match &self.form {
            LoginForm::Menu { .. } => self.on_menu_key(code),
            _ => self.on_field_key(code),
        }
    }

    fn on_menu_key(&mut self, code: KeyCode) -> Option<Action> {
        let selected = match &self.form {
            LoginForm::Menu { selected } => *selected,
            _ => return None,
        };
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let LoginForm::Menu { selected } = &mut self.form {
                    *selected = selected.saturating_sub(1);
                }
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let LoginForm::Menu { selected } = &mut self.form {
                    *selected = (*selected + 1).min(1);
                }
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Enter if selected == 0 => {
                self.qr_pending = true;
                self.busy = true;
                self.error = None;
                self.app_context.mark_dirty();
                Some(Action::LoginSelectQr)
            }
            KeyCode::Enter => {
                self.form = LoginForm::Phone {
                    text: String::new(),
                };
                self.error = None;
                self.app_context.mark_dirty();
                None
            }
            _ => None,
        }
    }

    fn on_field_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char(ch) => {
                if self.accept_char(ch) {
                    self.push_char(ch);
                    self.error = None;
                    self.app_context.mark_dirty();
                }
                None
            }
            KeyCode::Backspace => {
                self.pop_char();
                self.error = None;
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Tab => {
                if let LoginForm::Registration { field, .. } = &mut self.form {
                    *field = match *field {
                        NameField::First => NameField::Last,
                        NameField::Last => NameField::First,
                    };
                    self.app_context.mark_dirty();
                }
                None
            }
            KeyCode::Enter => self.submit_field(),
            _ => None,
        }
    }

    fn accept_char(&self, ch: char) -> bool {
        match &self.form {
            LoginForm::Phone { text } => {
                text.chars().count() < 32
                    && (ch.is_ascii_digit() || ch == '+' || ch == ' ' || ch == '-')
            }
            LoginForm::Code { text } | LoginForm::EmailCode { text } => {
                text.chars().count() < 16 && ch.is_ascii_alphanumeric()
            }
            LoginForm::Email { text } => text.chars().count() < 128 && !ch.is_control(),
            LoginForm::Password { .. } | LoginForm::Registration { .. } => {
                self.active_len() < 128 && !ch.is_control()
            }
            LoginForm::Menu { .. } => false,
        }
    }

    fn active_len(&self) -> usize {
        match &self.form {
            LoginForm::Registration { first, last, field } => match field {
                NameField::First => first.chars().count(),
                NameField::Last => last.chars().count(),
            },
            LoginForm::Phone { text }
            | LoginForm::Code { text }
            | LoginForm::Password { text }
            | LoginForm::Email { text }
            | LoginForm::EmailCode { text } => text.chars().count(),
            LoginForm::Menu { .. } => 0,
        }
    }

    fn push_char(&mut self, ch: char) {
        match &mut self.form {
            LoginForm::Phone { text }
            | LoginForm::Code { text }
            | LoginForm::Password { text }
            | LoginForm::Email { text }
            | LoginForm::EmailCode { text } => text.push(ch),
            LoginForm::Registration { first, last, field } => match field {
                NameField::First => first.push(ch),
                NameField::Last => last.push(ch),
            },
            LoginForm::Menu { .. } => {}
        }
    }

    fn pop_char(&mut self) {
        match &mut self.form {
            LoginForm::Phone { text }
            | LoginForm::Code { text }
            | LoginForm::Password { text }
            | LoginForm::Email { text }
            | LoginForm::EmailCode { text } => {
                text.pop();
            }
            LoginForm::Registration { first, last, field } => match field {
                NameField::First => {
                    first.pop();
                }
                NameField::Last => {
                    last.pop();
                }
            },
            LoginForm::Menu { .. } => {}
        }
    }

    fn insert_str(&mut self, pasted: &str) {
        for ch in pasted.chars() {
            if self.accept_char(ch) {
                self.push_char(ch);
            }
        }
        self.error = None;
        self.app_context.mark_dirty();
    }

    fn submit_field(&mut self) -> Option<Action> {
        let action = match &self.form {
            LoginForm::Phone { text } => {
                let phone = normalize_phone(text);
                if phone.len() < 5 {
                    self.error = Some("Include the country code, for example +1…".into());
                    self.app_context.mark_dirty();
                    return None;
                }
                Action::LoginSubmitPhone(phone)
            }
            LoginForm::Code { text } => {
                let code = text.trim().to_string();
                if code.is_empty() {
                    self.error = Some("Enter the code Telegram sent you.".into());
                    self.app_context.mark_dirty();
                    return None;
                }
                Action::LoginSubmitCode(code)
            }
            LoginForm::Password { text } => {
                if text.is_empty() {
                    self.error = Some("Enter your cloud password.".into());
                    self.app_context.mark_dirty();
                    return None;
                }
                Action::LoginSubmitPassword(text.clone())
            }
            LoginForm::Email { text } => {
                let email = text.trim().to_string();
                if !email.contains('@') {
                    self.error = Some("Enter an email address.".into());
                    self.app_context.mark_dirty();
                    return None;
                }
                Action::LoginSubmitEmail(email)
            }
            LoginForm::EmailCode { text } => {
                let code = text.trim().to_string();
                if code.is_empty() {
                    self.error = Some("Enter the email code.".into());
                    self.app_context.mark_dirty();
                    return None;
                }
                Action::LoginSubmitEmailCode(code)
            }
            LoginForm::Registration { .. } => {
                let (first, last, field) = match &self.form {
                    LoginForm::Registration { first, last, field } => {
                        (first.clone(), last.clone(), *field)
                    }
                    _ => return None,
                };
                if field == NameField::First {
                    if let LoginForm::Registration { field, .. } = &mut self.form {
                        *field = NameField::Last;
                    }
                    self.app_context.mark_dirty();
                    return None;
                }
                let first = first.trim().to_string();
                let last = last.trim().to_string();
                if first.is_empty() {
                    self.error = Some("First name is required.".into());
                    self.app_context.mark_dirty();
                    return None;
                }
                Action::LoginSubmitRegistration { first, last }
            }
            LoginForm::Menu { .. } => return None,
        };
        self.busy = true;
        self.error = None;
        self.app_context.mark_dirty();
        Some(action)
    }

    fn draw_card(&self, frame: &mut ratatui::Frame<'_>, area: Rect, lines: Vec<Line>, width: u16) {
        let card_width = width.min(area.width).max(1);
        let inner = usize::from(card_width.saturating_sub(2).max(1));
        let body_rows: usize = lines
            .iter()
            .map(|line| wrapped_rows(line_cols(line), inner))
            .sum();
        let body_rows = u16::try_from(body_rows).unwrap_or(u16::MAX);
        let card_height = body_rows.saturating_add(2).min(area.height).max(1);
        let rect = centered_rect(card_width, card_height, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.app_context.style_border_component_focused())
            .title(" Sign in ");
        let paragraph = Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, rect);
    }

    fn menu_lines(&self) -> Vec<Line<'_>> {
        let selected = match self.form {
            LoginForm::Menu { selected } => selected,
            _ => 0,
        };
        let options = ["Log in with a QR code", "Log in with a phone number"];
        let mut lines = vec![Line::from("Choose how to sign in"), Line::from("")];
        for (index, label) in options.iter().enumerate() {
            let marker = if index == selected { ">" } else { " " };
            let style = if index == selected {
                self.app_context.style_item_selected()
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!(" {marker}  {label}"),
                style,
            )));
        }
        lines.push(Line::from(""));
        lines.push(hint("↑↓ move    enter select    q quit"));
        self.push_error(&mut lines);
        lines
    }

    fn field_lines(
        &self,
        intro: &str,
        label: &str,
        value: &str,
        masked: bool,
        footer: &str,
    ) -> Vec<Line<'static>> {
        let shown = if masked {
            "•".repeat(value.chars().count())
        } else {
            value.to_string()
        };
        let mut lines = vec![
            Line::from(intro.to_string()),
            Line::from(""),
            Line::from(label.to_string()),
            Line::from(Span::styled(
                format!(" {shown}█"),
                self.app_context.style_item_selected(),
            )),
            Line::from(""),
            hint(footer),
        ];
        self.push_error(&mut lines);
        lines
    }

    fn registration_lines(&self) -> Vec<Line<'_>> {
        let (first, last, field) = match &self.form {
            LoginForm::Registration { first, last, field } => (first.clone(), last.clone(), *field),
            _ => (String::new(), String::new(), NameField::First),
        };
        let style_for = |active| {
            if active {
                self.app_context.style_item_selected()
            } else {
                Style::default()
            }
        };
        let mut lines = vec![
            Line::from("New account"),
            Line::from(""),
            Line::from("First name"),
            Line::from(Span::styled(
                format!(
                    " {first}{}",
                    if field == NameField::First { "█" } else { "" }
                ),
                style_for(field == NameField::First),
            )),
            Line::from("Last name"),
            Line::from(Span::styled(
                format!(" {last}{}", if field == NameField::Last { "█" } else { "" }),
                style_for(field == NameField::Last),
            )),
            Line::from(""),
            hint("tab switches fields    enter continues    ctrl-c quits"),
        ];
        self.push_error(&mut lines);
        lines
    }

    fn qr_lines(&self, link: &str, area_width: u16, area_height: u16) -> (Vec<Line<'static>>, u16) {
        let (planned, width) = plan_sign_in_qr(link, area_width, area_height);
        let mut lines = planned
            .into_iter()
            .map(|line| match line {
                PlannedLine::Text(text) => Line::from(text),
                PlannedLine::Dim(text) => hint(&text),
                PlannedLine::Blank => Line::from(""),
                PlannedLine::Qr(row) => Line::from(Span::styled(
                    row,
                    Style::default()
                        .fg(ratatui::style::Color::Black)
                        .bg(ratatui::style::Color::White),
                )),
            })
            .collect();
        self.push_error(&mut lines);
        (lines, width)
    }

    fn push_error<'a>(&self, lines: &mut Vec<Line<'a>>) {
        if let Some(error) = &self.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(ratatui::style::Color::Red),
            )));
        }
    }
}

impl HandleFocus for LoginWindow {
    fn focus(&mut self) {}
    fn unfocus(&mut self) {}
}

impl Component for LoginWindow {
    fn handle_events(&mut self, event: Option<Event>) -> Result<Option<Action>, AppError<Action>> {
        let action = match event {
            Some(Event::Key(code, modifiers)) => self.on_key(code, modifiers),
            Some(Event::Paste(text)) => {
                let auth = self.app_context.td_auth();
                self.sync_form(&auth);
                if !self.busy {
                    self.insert_str(&text);
                }
                None
            }
            _ => None,
        };
        Ok(action)
    }

    fn update(&mut self, action: Action) {
        if let Action::LoginFailed(message) = action {
            self.error = Some(message);
            self.busy = false;
            self.qr_pending = false;
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) -> io::Result<()> {
        let auth = self.app_context.td_auth();
        self.sync_form(&auth);
        let phone_text = match &self.form {
            LoginForm::Phone { text } => Some(text.clone()),
            _ => None,
        };
        let showing_phone = phone_text.is_some();

        let (lines, width) = match &auth {
            TdAuth::Starting => (
                vec![
                    Line::from("Connecting to Telegram…"),
                    Line::from(""),
                    hint("q quits"),
                ],
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitOtherDevice { link } => self.qr_lines(link, area.width, area.height),
            TdAuth::WaitPhoneNumber if self.qr_pending => (
                vec![
                    Line::from("Requesting a QR code…"),
                    Line::from(""),
                    hint("q quits"),
                ],
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitPhoneNumber if showing_phone => (
                self.field_lines(
                    "Phone number",
                    "Include the country code",
                    phone_text.as_deref().unwrap_or(""),
                    false,
                    "enter submits    esc back    ctrl-c quits",
                ),
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitPhoneNumber => (self.menu_lines(), TEXT_CARD_WIDTH),
            TdAuth::WaitCode => {
                let text = match &self.form {
                    LoginForm::Code { text } => text.clone(),
                    _ => String::new(),
                };
                (
                    self.field_lines(
                        "Verification code",
                        "Code from Telegram",
                        &text,
                        false,
                        "enter submits    ctrl-c quits",
                    ),
                    TEXT_CARD_WIDTH,
                )
            }
            TdAuth::WaitPassword => {
                let text = match &self.form {
                    LoginForm::Password { text } => text.clone(),
                    _ => String::new(),
                };
                (
                    self.field_lines(
                        "Cloud password",
                        "Two-step verification",
                        &text,
                        true,
                        "enter submits    ctrl-c quits",
                    ),
                    TEXT_CARD_WIDTH,
                )
            }
            TdAuth::WaitEmail => {
                let text = match &self.form {
                    LoginForm::Email { text } => text.clone(),
                    _ => String::new(),
                };
                (
                    self.field_lines(
                        "Email address",
                        "Telegram asked for an email",
                        &text,
                        false,
                        "enter submits    ctrl-c quits",
                    ),
                    TEXT_CARD_WIDTH,
                )
            }
            TdAuth::WaitEmailCode => {
                let text = match &self.form {
                    LoginForm::EmailCode { text } => text.clone(),
                    _ => String::new(),
                };
                (
                    self.field_lines(
                        "Email code",
                        "Code sent to your email",
                        &text,
                        false,
                        "enter submits    ctrl-c quits",
                    ),
                    TEXT_CARD_WIDTH,
                )
            }
            TdAuth::WaitRegistration => (self.registration_lines(), TEXT_CARD_WIDTH),
            TdAuth::Ready => (vec![Line::from("Signed in")], TEXT_CARD_WIDTH),
        };

        self.draw_card(frame, area, lines, width);
        Ok(())
    }
}

fn hint(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().add_modifier(Modifier::DIM),
    ))
}

fn line_cols(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| {
            let text: &str = span.content.as_ref();
            UnicodeWidthStr::width(text)
        })
        .sum()
}

fn wrapped_rows(cols: usize, inner: usize) -> usize {
    if cols == 0 || inner == 0 {
        1
    } else {
        cols.div_ceil(inner).max(1)
    }
}

/// One row of the sign-in card, before colors are applied.
#[derive(Debug, PartialEq, Eq)]
enum PlannedLine {
    Text(String),
    Dim(String),
    Blank,
    Qr(String),
}

/// Largest half-block QR that fits, with the most explanation that still leaves room.
///
/// Module size grows when the terminal has space, and the quiet zone shrinks when it
/// does not. A code that would have to be clipped is omitted.
fn plan_sign_in_qr(link: &str, area_width: u16, area_height: u16) -> (Vec<PlannedLine>, u16) {
    let inner_w = usize::from(area_width.saturating_sub(2));
    let inner_h = usize::from(area_height.saturating_sub(2));
    let presets: &[(&[&str], &[&str])] = &[
        (
            &[
                "Scan with Telegram on your phone",
                "Settings → Devices → Link Desktop Device",
                "",
            ],
            &["", "The code refreshes on its own.  q quits"],
        ),
        (&["Scan with Telegram", ""], &["", "q quits"]),
        (&["Scan with Telegram"], &["q quits"]),
        (&[], &["q quits"]),
        (&[], &[]),
    ];

    for (headers, footers) in presets {
        let chrome = headers.len() + footers.len();
        if chrome >= inner_h || inner_w < 21 {
            continue;
        }
        let Some(rows) = fit_qr(link, inner_w, inner_h - chrome) else {
            continue;
        };
        let qr_width = UnicodeWidthStr::width(rows[0].as_str());
        let min_field = usize::from(TEXT_CARD_WIDTH.saturating_sub(2)).min(inner_w);
        let field = qr_width.max(min_field).min(inner_w);
        let mut lines = Vec::new();
        push_copy(&mut lines, headers);
        let pad = field - qr_width;
        let left = pad / 2;
        let right = pad - left;
        for row in rows {
            lines.push(PlannedLine::Qr(format!(
                "{}{row}{}",
                " ".repeat(left),
                " ".repeat(right)
            )));
        }
        push_copy(&mut lines, footers);
        let link_rows = wrapped_rows(UnicodeWidthStr::width(link), field.max(1));
        if lines.len() + link_rows <= inner_h {
            lines.push(PlannedLine::Dim(link.to_string()));
        }
        let card_width = u16::try_from(field)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(area_width)
            .max(1);
        return (lines, card_width);
    }

    let width = TEXT_CARD_WIDTH.min(area_width).max(1);
    (
        vec![
            PlannedLine::Text("This window is too small for a QR code.".into()),
            PlannedLine::Blank,
            PlannedLine::Text("Make the terminal larger, or open the link".into()),
            PlannedLine::Text("on a phone that is already signed in.".into()),
            PlannedLine::Blank,
            PlannedLine::Dim(link.to_string()),
            PlannedLine::Blank,
            PlannedLine::Dim("q quits".into()),
        ],
        width,
    )
}

fn push_copy(lines: &mut Vec<PlannedLine>, texts: &[&str]) {
    for text in texts {
        if text.is_empty() {
            lines.push(PlannedLine::Blank);
        } else {
            lines.push(PlannedLine::Text((*text).to_string()));
        }
    }
}

const QR_SCALE_MAX: usize = 3;

fn fit_qr(data: &str, max_cols: usize, max_rows: usize) -> Option<Vec<String>> {
    for scale in (1..=QR_SCALE_MAX).rev() {
        for quiet in [4usize, 2, 1, 0] {
            for ec in [EcLevel::M, EcLevel::L] {
                let Some(rows) = render_qr(data, ec, quiet, scale) else {
                    continue;
                };
                let width = UnicodeWidthStr::width(rows[0].as_str());
                if width <= max_cols
                    && rows.len() <= max_rows
                    && rows
                        .iter()
                        .all(|row| UnicodeWidthStr::width(row.as_str()) == width)
                {
                    return Some(rows);
                }
            }
        }
    }
    None
}

fn render_qr(data: &str, ec: EcLevel, quiet: usize, scale: usize) -> Option<Vec<String>> {
    if scale == 0 {
        return None;
    }
    let code = QrCode::with_error_correction_level(data.as_bytes(), ec).ok()?;
    let modules = code.width();
    let pixels = modules.saturating_add(quiet.saturating_mul(2)) * scale;
    let mut rows = Vec::with_capacity(pixels.div_ceil(2));
    let mut y = 0;
    while y < pixels {
        let mut line = String::with_capacity(pixels);
        for x in 0..pixels {
            let top = module_dark(&code, x, y, modules, quiet, scale);
            let bottom = if y + 1 < pixels {
                module_dark(&code, x, y + 1, modules, quiet, scale)
            } else {
                false
            };
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        rows.push(line);
        y += 2;
    }
    Some(rows)
}

fn module_dark(
    code: &QrCode,
    x: usize,
    y: usize,
    modules: usize,
    quiet: usize,
    scale: usize,
) -> bool {
    let mx = x / scale;
    let my = y / scale;
    if mx < quiet || my < quiet {
        return false;
    }
    let mx = mx - quiet;
    let my = my - quiet;
    mx < modules && my < modules && code[(mx, my)] == QrColor::Dark
}

fn normalize_phone(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '+')
        .collect()
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

#[cfg(test)]
mod tests {
    use super::{fit_qr, normalize_phone, plan_sign_in_qr, PlannedLine};
    use unicode_width::UnicodeWidthStr;

    const LOGIN_LINK: &str = "tg://login?token=AQFMZ7FqnLbu6mkE73IL6xwZJc3jB9AK2eHmzKx733wi_g";

    fn qr_rows(lines: &[PlannedLine]) -> Vec<&str> {
        lines
            .iter()
            .filter_map(|line| match line {
                PlannedLine::Qr(row) => Some(row.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn qr_rows_share_one_width() {
        let rows = fit_qr(LOGIN_LINK, 80, 40).expect("qr");
        let width = UnicodeWidthStr::width(rows[0].as_str());
        assert!(width > 20);
        assert!(rows
            .iter()
            .all(|row| UnicodeWidthStr::width(row.as_str()) == width));
        assert!(rows
            .iter()
            .any(|row| row.contains('█') || row.contains('▀') || row.contains('▄')));
    }

    #[test]
    fn qr_fits_a_classic_terminal() {
        let (lines, card_width) = plan_sign_in_qr(LOGIN_LINK, 80, 24);
        let rows = qr_rows(&lines);
        assert!(!rows.is_empty());
        assert!(card_width <= 80);
        assert!(lines.len() + 2 <= 24);
        let width = UnicodeWidthStr::width(rows[0]);
        assert!(rows.iter().all(|row| UnicodeWidthStr::width(*row) == width));
        assert!(width + 2 <= 80);
    }

    #[test]
    fn qr_grows_when_the_terminal_is_larger() {
        let small = fit_qr(LOGIN_LINK, 76, 20).expect("small");
        let large = fit_qr(LOGIN_LINK, 180, 50).expect("large");
        let small_width = UnicodeWidthStr::width(small[0].as_str());
        let large_width = UnicodeWidthStr::width(large[0].as_str());
        assert!(large_width > small_width);
    }

    #[test]
    fn tiny_terminal_keeps_the_link_instead_of_a_clipped_code() {
        let (lines, card_width) = plan_sign_in_qr(LOGIN_LINK, 30, 12);
        assert!(qr_rows(&lines).is_empty());
        assert!(card_width <= 30);
        assert!(lines
            .iter()
            .any(|line| matches!(line, PlannedLine::Dim(text) if text.contains("tg://login"))));
    }

    #[test]
    fn phone_keeps_plus_and_digits() {
        assert_eq!(normalize_phone(" +1 555-0100 "), "+15550100");
    }
}
