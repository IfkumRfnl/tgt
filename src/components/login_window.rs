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
use std::{io, sync::Arc};
use unicode_width::UnicodeWidthStr;

const TEXT_CARD_WIDTH: u16 = 52;
/// Quiet-zone border around the QR matrix, in modules. Scanners need this
/// blank margin, so rendering never shrinks it to fit a small terminal.
const QR_QUIET: usize = 4;
/// Largest module scale tried first; larger codes scan more reliably.
const QR_MAX_SCALE: usize = 3;
/// Narrowest terminal that can still fit a version-1 QR code at scale 1.
/// Below this only the resize note fits, never a clipped code or token.
const QR_MIN_COLS: usize = 21;

/// Which prompt is accepting keystrokes. TDLib already says which step this is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Phone,
    Code,
    Password,
    Email,
    EmailCode,
    Name,
}

/// The two rows on the sign-in menu.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuRow {
    Qr,
    Phone,
}

/// QR encoding cached by link. Encoding runs once per rotated token; every
/// draw after that only picks a scale and paints half-block rows.
struct CachedQr {
    medium: Option<QrCode>,
    low: Option<QrCode>,
}

impl CachedQr {
    fn fresh(link: &str) -> Self {
        let bytes = link.as_bytes();
        Self {
            medium: QrCode::with_error_correction_level(bytes, EcLevel::M).ok(),
            low: QrCode::with_error_correction_level(bytes, EcLevel::L).ok(),
        }
    }
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
    menu: MenuRow,
    /// `WaitPhoneNumber` is showing the phone field rather than the menu.
    phone_entry: bool,
    error: Option<String>,
    /// A submit is in flight. On the menu this means the QR link was asked
    /// for and the card shows the requesting state instead of the menu.
    busy: bool,
    state: TdAuth,
    qr: Option<CachedQr>,
}

impl LoginWindow {
    pub fn new(app_context: Arc<AppContext>) -> Self {
        Self {
            app_context,
            text: String::new(),
            last: String::new(),
            on_last: false,
            menu: MenuRow::Qr,
            phone_entry: false,
            error: None,
            busy: false,
            state: TdAuth::Starting,
            qr: None,
        }
    }

    /// Drop every credential and token. [`crate::tui::Tui`] calls this when
    /// TDLib reports [`TdAuth::Ready`], once the card stops drawing.
    pub fn clear_secrets(&mut self) {
        self.reset_form();
        self.state = TdAuth::Ready;
    }

    /// Wipe typed input, errors and the QR cache without changing the step.
    fn reset_form(&mut self) {
        self.text = String::new();
        self.last = String::new();
        self.on_last = false;
        self.phone_entry = false;
        self.error = None;
        self.busy = false;
        self.qr = None;
    }

    /// Reset the form on a transition; encode each new QR token only once.
    fn sync_auth(&mut self) {
        let auth = {
            let auth = self.app_context.td_auth();
            if self.state == *auth {
                return;
            }
            auth.clone()
        };
        self.reset_form();
        self.qr = match &auth {
            TdAuth::WaitOtherDevice { link } => Some(CachedQr::fresh(link)),
            _ => None,
        };
        self.state = auth;
    }

    fn field_for(&self) -> Option<Field> {
        match self.state {
            TdAuth::WaitPhoneNumber if self.phone_entry => Some(Field::Phone),
            TdAuth::WaitCode => Some(Field::Code),
            TdAuth::WaitPassword => Some(Field::Password),
            TdAuth::WaitEmail => Some(Field::Email),
            TdAuth::WaitEmailCode => Some(Field::EmailCode),
            TdAuth::WaitRegistration => Some(Field::Name),
            _ => None,
        }
    }

    fn active_text(&mut self) -> &mut String {
        if self.on_last && matches!(self.state, TdAuth::WaitRegistration) {
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

        self.sync_auth();
        let on_menu = matches!(self.state, TdAuth::WaitPhoneNumber) && !self.phone_entry;
        let quit_pressed = matches!(code, KeyCode::Esc | KeyCode::Char('q'));
        if quit_pressed
            && (on_menu
                || matches!(
                    self.state,
                    TdAuth::Starting | TdAuth::WaitOtherDevice { .. }
                ))
        {
            return Some(Action::Quit);
        }
        if code == KeyCode::Esc
            && !self.busy
            && self.phone_entry
            && matches!(self.state, TdAuth::WaitPhoneNumber)
        {
            self.phone_entry = false;
            self.menu = MenuRow::Phone;
            self.error = None;
            self.app_context.mark_dirty();
            return None;
        }
        if self.busy {
            return None;
        }
        if on_menu {
            return self.on_menu_key(code);
        }
        self.on_field_key(code)
    }

    fn on_menu_key(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Up => self.menu = MenuRow::Qr,
            KeyCode::Down => self.menu = MenuRow::Phone,
            KeyCode::Enter => {
                self.error = None;
                self.app_context.mark_dirty();
                match self.menu {
                    MenuRow::Qr => {
                        self.busy = true;
                        return Some(Action::LoginSelectQr);
                    }
                    MenuRow::Phone => {
                        self.phone_entry = true;
                        self.text = String::new();
                        return None;
                    }
                }
            }
            _ => return None,
        }
        self.app_context.mark_dirty();
        None
    }

    fn on_field_key(&mut self, code: KeyCode) -> Option<Action> {
        let field = self.field_for()?;
        match code {
            KeyCode::Char(ch) => {
                let buffer = self.active_text();
                if accept_char(field, buffer, ch) {
                    buffer.push(ch);
                    self.error = None;
                    self.app_context.mark_dirty();
                }
                None
            }
            KeyCode::Backspace => {
                self.active_text().pop();
                self.error = None;
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Tab if matches!(field, Field::Name) => {
                self.on_last = !self.on_last;
                self.app_context.mark_dirty();
                None
            }
            KeyCode::Enter => self.submit(field),
            _ => None,
        }
    }

    fn insert_str(&mut self, pasted: &str) {
        let Some(field) = self.field_for() else {
            return;
        };
        for ch in pasted.chars() {
            let buffer = self.active_text();
            if accept_char(field, buffer, ch) {
                buffer.push(ch);
            }
        }
        self.error = None;
        self.app_context.mark_dirty();
    }

    fn submit(&mut self, field: Field) -> Option<Action> {
        if matches!(field, Field::Name) && !self.on_last {
            self.on_last = true;
            self.app_context.mark_dirty();
            return None;
        }
        let action = match field {
            Field::Phone => {
                let phone = normalize_phone(&self.text);
                if phone.len() < 5 {
                    return self.fail("Include the country code, for example +1…");
                }
                Action::LoginSubmitPhone(phone)
            }
            Field::Code => {
                let value = self.text.trim().to_string();
                if value.is_empty() {
                    return self.fail("Enter the code Telegram sent you.");
                }
                Action::LoginSubmitCode(value)
            }
            Field::EmailCode => {
                let value = self.text.trim().to_string();
                if value.is_empty() {
                    return self.fail("Enter the email code.");
                }
                Action::LoginSubmitEmailCode(value)
            }
            Field::Password => {
                if self.text.is_empty() {
                    return self.fail("Enter your cloud password.");
                }
                Action::LoginSubmitPassword(self.text.clone())
            }
            Field::Email => {
                let value = self.text.trim().to_string();
                if !value.contains('@') {
                    return self.fail("Enter an email address.");
                }
                Action::LoginSubmitEmail(value)
            }
            Field::Name => {
                let value = self.text.trim().to_string();
                if value.is_empty() {
                    return self.fail("First name is required.");
                }
                Action::LoginSubmitRegistration {
                    first: value,
                    last: self.last.trim().to_string(),
                }
            }
        };
        self.busy = true;
        self.error = None;
        self.app_context.mark_dirty();
        Some(action)
    }

    fn fail(&mut self, message: &str) -> Option<Action> {
        self.error = Some(message.to_string());
        self.app_context.mark_dirty();
        None
    }

    fn draw_card(&self, frame: &mut ratatui::Frame<'_>, area: Rect, lines: Vec<Line>, width: u16) {
        let card_width = width.min(area.width).max(1);
        let inner = usize::from(card_width.saturating_sub(2).max(1));
        let body_rows: usize = lines
            .iter()
            .map(|line| line.width().div_ceil(inner).max(1))
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
        let options = [
            (MenuRow::Qr, "Log in with a QR code"),
            (MenuRow::Phone, "Log in with a phone number"),
        ];
        let mut lines = vec![Line::from("Choose how to sign in"), Line::from("")];
        for (row, label) in options {
            let marker = if row == self.menu { ">" } else { " " };
            let style = if row == self.menu {
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
        push_error(&mut lines, self.error.as_deref());
        lines
    }

    fn field_lines(
        &self,
        intro: &'static str,
        label: &'static str,
        value: &str,
        masked: bool,
        footer: &'static str,
    ) -> Vec<Line<'static>> {
        let shown = if masked {
            "•".repeat(value.chars().count())
        } else {
            value.to_owned()
        };
        let mut lines = vec![
            Line::from(intro),
            Line::from(""),
            Line::from(label),
            Line::from(Span::styled(format!(" {shown}█"), active_style())),
            Line::from(""),
            hint(footer),
        ];
        push_error(&mut lines, self.error.as_deref());
        lines
    }

    fn registration_lines(&self) -> Vec<Line<'static>> {
        let row = |label: &'static str, value: &str, active: bool| {
            let cursor = if active { "█" } else { "" };
            let style = if active {
                active_style()
            } else {
                Style::default()
            };
            (
                Line::from(label),
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
        push_error(&mut lines, self.error.as_deref());
        lines
    }

    fn status_with_error(&self, message: &'static str) -> Vec<Line<'static>> {
        let mut lines = status_lines(message);
        push_error(&mut lines, self.error.as_deref());
        lines
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
                self.sync_auth();
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
        if matches!(*self.app_context.td_auth(), TdAuth::Ready) {
            self.clear_secrets();
            return;
        }
        if let Action::LoginFailed(message) = action {
            self.error = Some(message);
            self.busy = false;
            self.app_context.mark_dirty();
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) -> io::Result<()> {
        self.sync_auth();
        let (lines, width) = match &self.state {
            TdAuth::Starting => (
                self.status_with_error("Connecting to Telegram…"),
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitOtherDevice { .. } => qr_card_lines(
                self.qr.as_ref(),
                self.error.as_deref(),
                area.width,
                area.height,
            ),
            TdAuth::WaitPhoneNumber if self.busy && !self.phone_entry => (
                self.status_with_error("Requesting a QR code…"),
                TEXT_CARD_WIDTH,
            ),
            TdAuth::WaitPhoneNumber if self.phone_entry => (
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
            TdAuth::Ready => (status_lines("Signed in"), TEXT_CARD_WIDTH),
        };
        self.draw_card(frame, area, lines, width);
        Ok(())
    }
}

fn status_lines(message: &'static str) -> Vec<Line<'static>> {
    vec![Line::from(message), Line::from(""), hint("q quits")]
}

fn accept_char(field: Field, current: &str, ch: char) -> bool {
    let len = current.chars().count();
    match field {
        Field::Phone => {
            if ch.is_ascii_digit() || ch == ' ' || ch == '-' {
                len < 32
            } else {
                ch == '+'
                    && len < 32
                    && !current.contains('+')
                    && !current.chars().any(|cell| cell.is_ascii_digit())
            }
        }
        Field::Code | Field::EmailCode => len < 16 && ch.is_ascii_alphanumeric(),
        Field::Email => len < 128 && !ch.is_control() && !ch.is_whitespace(),
        Field::Password | Field::Name => len < 128 && !ch.is_control(),
    }
}

/// Black on white, so the active row stays readable on light and dark terminals.
fn active_style() -> Style {
    Style::default()
        .fg(ratatui::style::Color::Black)
        .bg(ratatui::style::Color::White)
        .add_modifier(Modifier::BOLD)
}

fn hint(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default().add_modifier(Modifier::DIM),
    ))
}

fn push_error(lines: &mut Vec<Line<'static>>, error: Option<&str>) {
    if let Some(message) = error {
        lines.push(Line::from(""));
        lines.extend(message.lines().map(|line| {
            Line::from(Span::styled(
                line.to_owned(),
                Style::default().fg(ratatui::style::Color::Red),
            ))
        }));
    }
}

/// QR card for the cached link. The raw link is only encoded, never printed:
/// a small terminal gets a resize note rather than a clipped code or token.
fn qr_card_lines(
    cached: Option<&CachedQr>,
    error: Option<&str>,
    area_width: u16,
    area_height: u16,
) -> (Vec<Line<'static>>, u16) {
    let inner_w = usize::from(area_width.saturating_sub(2));
    let inner_h = usize::from(area_height.saturating_sub(2));
    let error_rows = error.map_or(0, |message| 1 + message.lines().count());
    let error_width = error
        .into_iter()
        .flat_map(str::lines)
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0);
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

    if let Some(cached) = cached {
        for (headers, footers) in presets {
            let chrome = headers.len() + footers.len() + error_rows;
            if chrome >= inner_h || inner_w < QR_MIN_COLS {
                continue;
            }
            let Some(rows) = qr_rows(cached, inner_w, inner_h - chrome) else {
                continue;
            };
            let qr_width = UnicodeWidthStr::width(rows[0].as_str());
            let min_field = usize::from(TEXT_CARD_WIDTH.saturating_sub(2)).min(inner_w);
            let field = qr_width.max(min_field).max(error_width).min(inner_w);
            // Keep each QR-card line on one terminal row so wrapping cannot clip the code.
            if error_width > field
                || headers
                    .iter()
                    .chain(footers.iter())
                    .any(|text| text.width() > field)
            {
                continue;
            }
            let mut lines = Vec::new();
            for text in *headers {
                lines.push(Line::from(*text));
            }
            let pad = field.saturating_sub(qr_width);
            let left = pad / 2;
            for row in rows {
                lines.push(Line::from(Span::styled(
                    format!("{}{row}{}", " ".repeat(left), " ".repeat(pad - left)),
                    active_style(),
                )));
            }
            for text in *footers {
                lines.push(Line::from(*text));
            }
            push_error(&mut lines, error);
            let card_width = u16::try_from(field)
                .unwrap_or(u16::MAX)
                .saturating_add(2)
                .min(area_width)
                .max(1);
            return (lines, card_width);
        }
    }

    resize_lines(error, area_width)
}

fn resize_lines(error: Option<&str>, area_width: u16) -> (Vec<Line<'static>>, u16) {
    let width = TEXT_CARD_WIDTH.min(area_width).max(1);
    let mut lines = vec![
        Line::from("This window is too small for a QR code."),
        Line::from(""),
        Line::from("Make the terminal larger to sign in."),
        Line::from(""),
        hint("q quits"),
    ];
    push_error(&mut lines, error);
    (lines, width)
}

/// Largest readable rendering wins: scale first, then the medium-error-
/// correction encoding before the smaller low one. The quiet zone always
/// stays four modules.
fn qr_rows(cached: &CachedQr, max_cols: usize, max_rows: usize) -> Option<Vec<String>> {
    for scale in (1..=QR_MAX_SCALE).rev() {
        for code in cached.medium.iter().chain(cached.low.iter()) {
            let pixels = code
                .width()
                .saturating_add(QR_QUIET.saturating_mul(2))
                .saturating_mul(scale);
            if pixels <= max_cols && pixels.div_ceil(2) <= max_rows {
                return Some(paint_qr(code, scale));
            }
        }
    }
    None
}

fn paint_qr(code: &QrCode, scale: usize) -> Vec<String> {
    let modules = code.width();
    let pixels = modules
        .saturating_add(QR_QUIET.saturating_mul(2))
        .saturating_mul(scale);
    let dark = |x: usize, y: usize| {
        let mx = x / scale;
        let my = y / scale;
        mx >= QR_QUIET
            && my >= QR_QUIET
            && mx - QR_QUIET < modules
            && my - QR_QUIET < modules
            && code[(mx - QR_QUIET, my - QR_QUIET)] == QrColor::Dark
    };
    let mut rows = Vec::with_capacity(pixels.div_ceil(2));
    for y in (0..pixels).step_by(2) {
        let mut line = String::with_capacity(pixels);
        for x in 0..pixels {
            let top = dark(x, y);
            let bottom = y + 1 < pixels && dark(x, y + 1);
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        rows.push(line);
    }
    rows
}

fn normalize_phone(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_digit() || (ch == '+' && out.is_empty()) {
            out.push(ch);
        }
    }
    out
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
    use super::{qr_card_lines, qr_rows, CachedQr, LoginWindow};
    use crate::{
        action::Action,
        components::{component_traits::Component, search_tests::create_test_app_context},
        event::Event,
        tg::login_phase::TdAuth,
    };
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::text::Line;
    use ratatui::{backend::TestBackend, Terminal};
    use unicode_width::UnicodeWidthStr;

    const LOGIN_LINK: &str = "tg://login?token=0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJK";

    fn line_string(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn has_blocks(text: &str) -> bool {
        text.contains('█') || text.contains('▀') || text.contains('▄')
    }

    fn is_qr(line: &Line<'_>) -> bool {
        has_blocks(&line_string(line))
    }

    #[test]
    fn qr_fits_a_classic_terminal() {
        let cached = CachedQr::fresh(LOGIN_LINK);
        let (lines, card_width) = qr_card_lines(Some(&cached), None, 80, 24);
        let rows: Vec<_> = lines.iter().filter(|line| is_qr(line)).collect();
        assert!(!rows.is_empty());
        assert!(card_width <= 80);
        assert!(lines.len() + 2 <= 24);
        let width = UnicodeWidthStr::width(line_string(rows[0]).as_str());
        assert!(rows
            .iter()
            .all(|line| UnicodeWidthStr::width(line_string(line).as_str()) == width));
        assert!(width + 2 <= 80);
        assert!(lines
            .iter()
            .all(|line| !line_string(line).contains("tg://")));
    }

    #[test]
    fn qr_keeps_four_module_quiet_zone() {
        let cached = CachedQr::fresh(LOGIN_LINK);
        let rows = qr_rows(&cached, 80, 22).expect("qr");
        assert!(!has_blocks(&rows[0]));
        assert!(!has_blocks(&rows[1]));
        assert!(!has_blocks(&rows[rows.len() - 1]));
        assert!(!has_blocks(&rows[rows.len() - 2]));
        assert!(rows
            .iter()
            .all(|row| row.starts_with("    ") && row.ends_with("    ")));
    }

    #[test]
    fn tiny_terminal_shows_resize_instead_of_code() {
        let cached = CachedQr::fresh(LOGIN_LINK);
        for (width, height) in [(30, 12), (80, 10)] {
            let (lines, card_width) = qr_card_lines(Some(&cached), None, width, height);
            assert!(lines.iter().all(|line| !is_qr(line)));
            assert!(lines
                .iter()
                .all(|line| !line_string(line).contains("tg://")));
            assert!(card_width <= width);
        }
    }

    #[test]
    fn qr_card_reserves_space_for_wide_multiline_errors() {
        let cached = CachedQr::fresh(LOGIN_LINK);
        let error = format!("{}\nAdditional detail", "x".repeat(60));
        let (lines, width) = qr_card_lines(Some(&cached), Some(&error), 80, 30);
        assert!(lines.iter().any(is_qr));
        assert!(lines
            .iter()
            .all(|line| line.width() <= usize::from(width - 2)));
        assert!(lines.len() + 2 <= 30);

        let (lines, _) = qr_card_lines(Some(&cached), Some(&error), 80, 24);
        assert!(lines.iter().all(|line| !is_qr(line)));
    }

    #[test]
    fn qr_request_waits_for_response_and_can_retry_after_failure() {
        let context = create_test_app_context();
        *context.td_auth() = TdAuth::WaitPhoneNumber;
        let mut login = LoginWindow::new(context);
        assert!(matches!(
            login.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::LoginSelectQr)
        ));
        assert!(login.on_key(KeyCode::Enter, KeyModifiers::NONE).is_none());
        login.update(Action::LoginFailed("Request rejected".into()));
        assert!(matches!(
            login.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::LoginSelectQr)
        ));
    }

    #[test]
    fn password_is_masked_submitted_verbatim_and_cleared_after_ready() {
        let context = create_test_app_context();
        *context.td_auth() = TdAuth::WaitPassword;
        let mut login = LoginWindow::new(context.clone());
        let password = " secret with spaces ";
        login
            .handle_events(Some(Event::Paste(password.into())))
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| login.draw(frame, frame.area()).unwrap())
            .unwrap();
        let buffer = terminal.backend().buffer();
        let visible: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(!visible.contains("secret"));
        assert!(visible.contains(&"•".repeat(password.chars().count())));
        assert!(matches!(
            login.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::LoginSubmitPassword(value)) if value == password
        ));
        *context.td_auth() = TdAuth::Ready;
        login.clear_secrets();
        *context.td_auth() = TdAuth::WaitPassword;
        assert!(login.on_key(KeyCode::Enter, KeyModifiers::NONE).is_none());
    }
}
