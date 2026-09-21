use crate::{
    action::Action,
    app_context::AppContext,
    app_error::AppError,
    components::component_traits::{Component, HandleFocus},
    event::Event,
    tg::login_phase::TdAuth,
};
use crossterm::event::{KeyCode, KeyModifiers};
use qrcode::{Color, QrCode};
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
        let body_rows = lines.len() as u16;
        let card_width = width.min(area.width).max(1);
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

    fn qr_lines(&self, link: &str, max_body_rows: u16) -> (Vec<Line<'static>>, u16) {
        let rendered = [2usize, 1, 0].into_iter().find_map(|quiet| {
            let rows = render_qr_half_blocks_quiet(link, quiet)?;
            let chrome = 6u16;
            if rows.len() as u16 + chrome <= max_body_rows || quiet == 0 {
                Some(rows)
            } else {
                None
            }
        });
        let mut lines = vec![
            Line::from("Scan with Telegram on your phone"),
            Line::from("Settings → Devices → Link Desktop Device"),
            Line::from(""),
        ];
        let mut width = TEXT_CARD_WIDTH;
        match rendered {
            Some(rows) => {
                let row_width = rows
                    .first()
                    .map(|row| UnicodeWidthStr::width(row.as_str()))
                    .unwrap_or(0);
                width = (row_width as u16).saturating_add(4).max(TEXT_CARD_WIDTH);
                for row in rows {
                    lines.push(Line::from(Span::styled(
                        format!(" {row}"),
                        Style::default()
                            .fg(ratatui::style::Color::Black)
                            .bg(ratatui::style::Color::White),
                    )));
                }
            }
            None => {
                lines.push(Line::from("Could not build a QR code for this link."));
            }
        }
        lines.push(Line::from(""));
        lines.push(hint("The code refreshes on its own.  q quits"));
        if lines.len() as u16 + 1 < max_body_rows {
            lines.push(hint(link));
        }
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
            TdAuth::WaitOtherDevice { link } => self.qr_lines(link, area.height.saturating_sub(2)),
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

/// Render `data` as Unicode half-block rows, with a 2-module quiet zone.
pub fn render_qr_half_blocks(data: &str) -> Option<Vec<String>> {
    render_qr_half_blocks_quiet(data, 2)
}

fn render_qr_half_blocks_quiet(data: &str, quiet: usize) -> Option<Vec<String>> {
    let code = QrCode::new(data.as_bytes()).ok()?;
    let modules = code.width();
    let size = modules + quiet * 2;
    let mut dark = vec![vec![false; size]; size];
    for y in 0..modules {
        for x in 0..modules {
            if code[(x, y)] == Color::Dark {
                dark[y + quiet][x + quiet] = true;
            }
        }
    }

    let mut rows = Vec::new();
    let mut y = 0;
    while y < size {
        let mut line = String::new();
        for (x, cell) in dark[y].iter().enumerate() {
            let top = *cell;
            let bottom = if y + 1 < size { dark[y + 1][x] } else { false };
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

#[cfg(test)]
mod tests {
    use super::{normalize_phone, render_qr_half_blocks};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn qr_rows_are_even_blocks() {
        let rows = render_qr_half_blocks("tg://login?token=abc").expect("qr");
        assert!(rows.len() >= 10);
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
    fn phone_keeps_plus_and_digits() {
        assert_eq!(normalize_phone(" +1 555-0100 "), "+15550100");
    }
}
