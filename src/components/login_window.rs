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

/// Which prompt is accepting keystrokes. TDLib already says which step this is.
#[derive(Clone, Copy)]
enum Input {
    Phone,
    Code,
    Password,
    Email,
    EmailCode,
    Name,
}

/// Sign-in card shown before [`TdAuth::Ready`].
pub struct LoginWindow {
    app_context: Arc<AppContext>,
    /// Current field. During registration this is the first name.
    text: String,
    /// Last name. Used only while registering.
    last: String,
    /// Registration cursor is on the last name.
    on_last: bool,
    /// Menu cursor: 0 is QR, 1 is phone.
    menu: usize,
    /// `WaitPhoneNumber` is showing the phone field rather than the menu.
    entering_phone: bool,
    error: Option<String>,
    /// QR was requested and the link has not arrived yet.
    qr_pending: bool,
    busy: bool,
    seen: Discriminant<TdAuth>,
}

impl LoginWindow {
    pub fn new(app_context: Arc<AppContext>) -> Self {
        Self {
            app_context,
            text: String::new(),
            last: String::new(),
            on_last: false,
            menu: 0,
            entering_phone: false,
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
        self.text.clear();
        self.last.clear();
        self.on_last = false;
        self.entering_phone = false;
        self.error = None;
        self.qr_pending = false;
        self.busy = false;
    }

    fn input(&self, auth: &TdAuth) -> Option<Input> {
        match auth {
            TdAuth::WaitPhoneNumber if self.entering_phone => Some(Input::Phone),
            TdAuth::WaitCode => Some(Input::Code),
            TdAuth::WaitPassword => Some(Input::Password),
            TdAuth::WaitEmail => Some(Input::Email),
            TdAuth::WaitEmailCode => Some(Input::EmailCode),
            TdAuth::WaitRegistration => Some(Input::Name),
            _ => None,
        }
    }

    fn buffer(&mut self, auth: &TdAuth) -> &mut String {
        if self.on_last && matches!(auth, TdAuth::WaitRegistration) {
            &mut self.last
        } else {
            &mut self.text
        }
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        if modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('c') | KeyCode::Char('q'))
        {
            return Some(Action::Quit);
        }

        let auth = self.app_context.td_auth();
        self.sync_form(&auth);
        let on_menu = matches!(auth, TdAuth::WaitPhoneNumber) && !self.entering_phone;
        let quits = self.qr_pending
            || on_menu
            || matches!(auth, TdAuth::Starting | TdAuth::WaitOtherDevice { .. });
        if quits && matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
            return Some(Action::Quit);
        }
        if code == KeyCode::Esc && self.entering_phone {
            self.entering_phone = false;
            self.menu = 1;
            self.error = None;
            self.app_context.mark_dirty();
            return None;
        }
        if self.busy {
            return None;
        }
        if on_menu {
            self.on_menu_key(code)
        } else {
            self.on_field_key(code, &auth)
        }
    }

    fn on_menu_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.menu = self.menu.saturating_sub(1);
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu = (self.menu + 1).min(1);
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Enter if self.menu == 0 => {
                self.qr_pending = true;
                self.busy = true;
                self.error = None;
                self.app_context.mark_dirty();
                Some(Action::LoginSelectQr)
            }
            KeyCode::Enter => {
                self.entering_phone = true;
                self.text.clear();
                self.error = None;
                self.app_context.mark_dirty();
                None
            }
            _ => None,
        }
    }

    fn on_field_key(&mut self, code: KeyCode, auth: &TdAuth) -> Option<Action> {
        let input = self.input(auth)?;
        match code {
            KeyCode::Char(ch) => {
                let len = self.buffer(auth).chars().count();
                if accept_char(input, len, ch) {
                    self.buffer(auth).push(ch);
                    self.error = None;
                    self.app_context.mark_dirty();
                }
                None
            }
            KeyCode::Backspace => {
                self.buffer(auth).pop();
                self.error = None;
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Tab if matches!(input, Input::Name) => {
                self.on_last = !self.on_last;
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Enter => self.submit(input),
            _ => None,
        }
    }

    fn insert_str(&mut self, auth: &TdAuth, pasted: &str) {
        let Some(input) = self.input(auth) else {
            return;
        };
        for ch in pasted.chars() {
            let len = self.buffer(auth).chars().count();
            if accept_char(input, len, ch) {
                self.buffer(auth).push(ch);
            }
        }
        self.error = None;
        self.app_context.mark_dirty();
    }

    fn submit(&mut self, input: Input) -> Option<Action> {
        if matches!(input, Input::Name) && !self.on_last {
            self.on_last = true;
            self.app_context.mark_dirty();
            return None;
        }
        let value = self.text.trim().to_string();
        let action = match input {
            Input::Phone => {
                let phone = normalize_phone(&self.text);
                if phone.len() < 5 {
                    return self.fail("Include the country code, for example +1…");
                }
                Action::LoginSubmitPhone(phone)
            }
            Input::Code => {
                if value.is_empty() {
                    return self.fail("Enter the code Telegram sent you.");
                }
                Action::LoginSubmitCode(value)
            }
            Input::EmailCode => {
                if value.is_empty() {
                    return self.fail("Enter the email code.");
                }
                Action::LoginSubmitEmailCode(value)
            }
            Input::Password => {
                if self.text.is_empty() {
                    return self.fail("Enter your cloud password.");
                }
                Action::LoginSubmitPassword(self.text.clone())
            }
            Input::Email => {
                if !value.contains('@') {
                    return self.fail("Enter an email address.");
                }
                Action::LoginSubmitEmail(value)
            }
            Input::Name => {
                if value.is_empty() {
                    return self.fail("First name is required.");
                }
                Action::LoginSubmitRegistration {
                    first: value,
                    last: self.last.trim().to_string(),
                }
            }
        };
        self.finish(action)
    }

    fn fail(&mut self, message: &str) -> Option<Action> {
        self.error = Some(message.to_string());
        self.app_context.mark_dirty();
        None
    }

    fn finish(&mut self, action: Action) -> Option<Action> {
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
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.app_context.style_border_component_focused())
            .title(" Sign in ");
        let paragraph = Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, centered_rect(card_width, card_height, area));
    }

    fn menu_lines(&self) -> Vec<Line<'static>> {
        let options = ["Log in with a QR code", "Log in with a phone number"];
        let mut lines = vec![Line::from("Choose how to sign in"), Line::from("")];
        for (index, label) in options.iter().enumerate() {
            let marker = if index == self.menu { ">" } else { " " };
            let style = if index == self.menu {
                active_style()
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
            Line::from(Span::styled(format!(" {shown}█"), active_style())),
            Line::from(""),
            hint(footer),
        ];
        self.push_error(&mut lines);
        lines
    }

    fn registration_lines(&self) -> Vec<Line<'static>> {
        let row = |label: &str, value: &str, active: bool| {
            let cursor = if active { "█" } else { "" };
            let style = if active {
                active_style()
            } else {
                Style::default()
            };
            (
                Line::from(label.to_string()),
                Line::from(Span::styled(format!(" {value}{cursor}"), style)),
            )
        };
        let (first_label, first_row) = row("First name", &self.text, !self.on_last);
        let (last_label, last_row) = row("Last name", &self.last, self.on_last);
        let mut lines = vec![
            Line::from("New account"),
            Line::from(""),
            first_label,
            first_row,
            last_label,
            last_row,
            Line::from(""),
            hint("tab switches fields    enter continues    ctrl-c quits"),
        ];
        self.push_error(&mut lines);
        lines
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
                    self.insert_str(&auth, &text);
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
        let (mut lines, width) = match &auth {
            TdAuth::Starting => (status_lines("Connecting to Telegram…"), TEXT_CARD_WIDTH),
            TdAuth::WaitOtherDevice { link } => qr_lines(link, area.width, area.height),
            TdAuth::WaitPhoneNumber if self.qr_pending => {
                (status_lines("Requesting a QR code…"), TEXT_CARD_WIDTH)
            }
            TdAuth::WaitPhoneNumber if self.entering_phone => (
                self.field_lines(
                    "Phone number",
                    "Include the country code",
                    &self.text,
                    false,
                    "enter submits    esc back    ctrl-c quits",
                ),
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitPhoneNumber => (self.menu_lines(), TEXT_CARD_WIDTH),
            TdAuth::WaitCode => (
                self.field_lines(
                    "Verification code",
                    "Code from Telegram",
                    &self.text,
                    false,
                    "enter submits    ctrl-c quits",
                ),
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitPassword => (
                self.field_lines(
                    "Cloud password",
                    "Two-step verification",
                    &self.text,
                    true,
                    "enter submits    ctrl-c quits",
                ),
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitEmail => (
                self.field_lines(
                    "Email address",
                    "Telegram asked for an email",
                    &self.text,
                    false,
                    "enter submits    ctrl-c quits",
                ),
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitEmailCode => (
                self.field_lines(
                    "Email code",
                    "Code sent to your email",
                    &self.text,
                    false,
                    "enter submits    ctrl-c quits",
                ),
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitRegistration => (self.registration_lines(), TEXT_CARD_WIDTH),
            TdAuth::Ready => (vec![Line::from("Signed in")], TEXT_CARD_WIDTH),
        };
        if matches!(auth, TdAuth::WaitOtherDevice { .. }) {
            self.push_error(&mut lines);
        }
        self.draw_card(frame, area, lines, width);
        Ok(())
    }
}

fn status_lines(message: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(message.to_string()),
        Line::from(""),
        hint("q quits"),
    ]
}

fn accept_char(input: Input, len: usize, ch: char) -> bool {
    match input {
        Input::Phone => len < 32 && (ch.is_ascii_digit() || ch == '+' || ch == ' ' || ch == '-'),
        Input::Code | Input::EmailCode => len < 16 && ch.is_ascii_alphanumeric(),
        Input::Email => len < 128 && !ch.is_control(),
        Input::Password | Input::Name => len < 128 && !ch.is_control(),
    }
}

/// Black on white, so the active row stays readable on light and dark terminals.
fn active_style() -> Style {
    ink().add_modifier(Modifier::BOLD)
}

fn ink() -> Style {
    Style::default()
        .fg(ratatui::style::Color::Black)
        .bg(ratatui::style::Color::White)
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

fn qr_lines(link: &str, area_width: u16, area_height: u16) -> (Vec<Line<'static>>, u16) {
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
        push_plain(&mut lines, headers);
        let pad = field - qr_width;
        let left = pad / 2;
        for row in rows {
            lines.push(Line::from(Span::styled(
                format!("{}{row}{}", " ".repeat(left), " ".repeat(pad - left)),
                ink(),
            )));
        }
        push_plain(&mut lines, footers);
        let link_rows = wrapped_rows(UnicodeWidthStr::width(link), field.max(1));
        if lines.len() + link_rows <= inner_h {
            lines.push(hint(link));
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
            Line::from("This window is too small for a QR code."),
            Line::from(""),
            Line::from("Make the terminal larger, or open the link"),
            Line::from("on a phone that is already signed in."),
            Line::from(""),
            hint(link),
            Line::from(""),
            hint("q quits"),
        ],
        width,
    )
}

fn push_plain(lines: &mut Vec<Line<'static>>, texts: &[&str]) {
    for text in texts {
        lines.push(if text.is_empty() {
            Line::from("")
        } else {
            Line::from((*text).to_string())
        });
    }
}

fn fit_qr(data: &str, max_cols: usize, max_rows: usize) -> Option<Vec<String>> {
    let medium = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).ok();
    let low = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L).ok();
    for scale in (1..=3).rev() {
        for quiet in [4usize, 2, 1, 0] {
            for code in medium.iter().chain(low.iter()) {
                let modules = code.width();
                let pixels = modules
                    .saturating_add(quiet.saturating_mul(2))
                    .saturating_mul(scale);
                if pixels <= max_cols && pixels.div_ceil(2) <= max_rows {
                    return Some(paint_qr(code, modules, quiet, scale));
                }
            }
        }
    }
    None
}

fn paint_qr(code: &QrCode, modules: usize, quiet: usize, scale: usize) -> Vec<String> {
    let pixels = modules
        .saturating_add(quiet.saturating_mul(2))
        .saturating_mul(scale);
    let mut rows = Vec::with_capacity(pixels.div_ceil(2));
    let mut y = 0;
    while y < pixels {
        let mut line = String::with_capacity(pixels);
        for x in 0..pixels {
            let top = module_dark(code, x, y, modules, quiet, scale);
            let bottom = y + 1 < pixels && module_dark(code, x, y + 1, modules, quiet, scale);
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
    rows
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
    use super::{fit_qr, normalize_phone, qr_lines};
    use ratatui::text::Line;
    use unicode_width::UnicodeWidthStr;

    const LOGIN_LINK: &str = "tg://login?token=AQFMZ7FqnLbu6mkE73IL6xwZJc3jB9AK2eHmzKx733wi_g";

    fn line_string(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| {
                let text: &str = span.content.as_ref();
                text
            })
            .collect()
    }

    fn has_blocks(text: &str) -> bool {
        text.contains('█') || text.contains('▀') || text.contains('▄')
    }

    fn is_qr(line: &Line<'_>) -> bool {
        has_blocks(&line_string(line))
    }

    #[test]
    fn qr_rows_share_one_width() {
        let rows = fit_qr(LOGIN_LINK, 80, 40).expect("qr");
        let width = UnicodeWidthStr::width(rows[0].as_str());
        assert!(width > 20);
        assert!(rows
            .iter()
            .all(|row| UnicodeWidthStr::width(row.as_str()) == width));
        assert!(rows.iter().any(|row| has_blocks(row)));
    }

    #[test]
    fn qr_fits_a_classic_terminal() {
        let (lines, card_width) = qr_lines(LOGIN_LINK, 80, 24);
        let rows: Vec<_> = lines.iter().filter(|line| is_qr(line)).collect();
        assert!(!rows.is_empty());
        assert!(card_width <= 80);
        assert!(lines.len() + 2 <= 24);
        let width = UnicodeWidthStr::width(line_string(rows[0]).as_str());
        assert!(rows
            .iter()
            .all(|line| UnicodeWidthStr::width(line_string(line).as_str()) == width));
        assert!(width + 2 <= 80);
    }

    #[test]
    fn qr_grows_when_the_terminal_is_larger() {
        let small = fit_qr(LOGIN_LINK, 76, 20).expect("small");
        let large = fit_qr(LOGIN_LINK, 180, 50).expect("large");
        assert!(
            UnicodeWidthStr::width(large[0].as_str()) > UnicodeWidthStr::width(small[0].as_str())
        );
    }

    #[test]
    fn tiny_terminal_keeps_the_link_instead_of_a_clipped_code() {
        let (lines, card_width) = qr_lines(LOGIN_LINK, 30, 12);
        assert!(lines.iter().all(|line| !is_qr(line)));
        assert!(card_width <= 30);
        assert!(lines
            .iter()
            .any(|line| line_string(line).contains("tg://login")));
    }

    #[test]
    fn phone_keeps_plus_and_digits() {
        assert_eq!(normalize_phone(" +1 555-0100 "), "+15550100");
    }
}
