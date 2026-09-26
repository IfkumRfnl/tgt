use crate::{
    action::{Action, LoginRequest},
    app_context::AppContext,
    app_error::AppError,
    components::component_traits::{Component, HandleFocus},
    event::Event,
    tg::login_phase::TdAuth,
};
use crossterm::event::{KeyCode, KeyModifiers};
use qrcode::{Color as QrColor, EcLevel, QrCode};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use std::{borrow::Cow, io, sync::Arc};
use unicode_width::UnicodeWidthStr;

const TEXT_W: u16 = 52;
const QUIET: usize = 4;

/// Encode once per token; prefer medium correction, falling back to low to fit.
struct CachedQr {
    medium: Option<QrCode>,
    low: Option<QrCode>,
}

impl CachedQr {
    fn fresh(link: &str) -> Self {
        Self {
            medium: QrCode::with_error_correction_level(link.as_bytes(), EcLevel::M).ok(),
            low: QrCode::with_error_correction_level(link.as_bytes(), EcLevel::L).ok(),
        }
    }

    fn pick(&self, width: usize, height: usize) -> Option<(&QrCode, usize)> {
        for scale in (1..=3).rev() {
            for code in self.medium.iter().chain(self.low.iter()) {
                let pixels = (code.width() + QUIET * 2) * scale;
                if pixels <= width && pixels.div_ceil(2) <= height {
                    return Some((code, scale));
                }
            }
        }
        None
    }
}

/// TDLib owns the authorization step; the component owns input and presentation.
pub struct LoginWindow {
    app_context: Arc<AppContext>,
    state: TdAuth,
    text: String,
    last: String,
    on_last: bool,
    qr_selected: bool,
    phone_entry: bool,
    busy: bool,
    error: Option<String>,
    qr: Option<CachedQr>,
}

impl LoginWindow {
    pub fn new(app_context: Arc<AppContext>) -> Self {
        Self {
            app_context,
            state: TdAuth::Starting,
            text: String::new(),
            last: String::new(),
            on_last: false,
            qr_selected: true,
            phone_entry: false,
            busy: false,
            error: None,
            qr: None,
        }
    }

    fn sync_auth(&mut self) {
        let state = {
            let state = self.app_context.td_auth();
            if *state == self.state {
                return;
            }
            state.clone()
        };
        // Drop credential buffers on every transition, including Ready.
        self.text = String::new();
        self.last = String::new();
        self.on_last = false;
        self.phone_entry = false;
        self.busy = false;
        self.error = None;
        self.qr = match &state {
            TdAuth::WaitOtherDevice { link } => Some(CachedQr::fresh(link)),
            _ => None,
        };
        self.state = state;
    }

    fn editing(&self) -> bool {
        match self.state {
            TdAuth::WaitPhoneNumber => self.phone_entry,
            TdAuth::Starting | TdAuth::WaitOtherDevice { .. } | TdAuth::Ready => false,
            _ => true,
        }
    }

    fn insert_str(&mut self, text: &str) {
        if !self.editing() {
            return;
        }
        let input = if self.on_last {
            &mut self.last
        } else {
            &mut self.text
        };
        let mut len = input.chars().count();
        for ch in text.chars() {
            let accepted = match self.state {
                TdAuth::WaitPhoneNumber => {
                    len < 32
                        && (ch.is_ascii_digit()
                            || matches!(ch, ' ' | '-')
                            || (ch == '+'
                                && !input.contains('+')
                                && !input.chars().any(|ch| ch.is_ascii_digit())))
                }
                TdAuth::WaitCode | TdAuth::WaitEmailCode => len < 16 && ch.is_ascii_alphanumeric(),
                TdAuth::WaitEmail => len < 128 && !ch.is_control() && !ch.is_whitespace(),
                _ => len < 128 && !ch.is_control(),
            };
            if accepted {
                input.push(ch);
                len += 1;
            }
        }
        self.error = None;
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        self.sync_auth();
        let menu = matches!(self.state, TdAuth::WaitPhoneNumber) && !self.phone_entry;
        let editing = self.editing();
        if (modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c' | 'q')))
            || (matches!(code, KeyCode::Esc | KeyCode::Char('q')) && !editing)
        {
            return Some(Action::Quit);
        }
        if self.busy {
            return None;
        }
        self.app_context.mark_dirty();
        if menu {
            match code {
                KeyCode::Up => self.qr_selected = true,
                KeyCode::Down => self.qr_selected = false,
                KeyCode::Enter if self.qr_selected => {
                    self.busy = true;
                    self.error = None;
                    return Some(Action::Login(LoginRequest::Qr));
                }
                KeyCode::Enter => {
                    self.phone_entry = true;
                    self.text.clear();
                    self.error = None;
                }
                _ => {}
            }
        } else if editing {
            match code {
                KeyCode::Esc if self.phone_entry => {
                    self.phone_entry = false;
                    self.qr_selected = false;
                    self.error = None;
                }
                KeyCode::Char(ch) => self.insert_str(ch.encode_utf8(&mut [0; 4])),
                KeyCode::Backspace => {
                    if self.on_last {
                        self.last.pop();
                    } else {
                        self.text.pop();
                    }
                    self.error = None;
                }
                KeyCode::Tab if matches!(self.state, TdAuth::WaitRegistration) => {
                    self.on_last = !self.on_last;
                }
                KeyCode::Enter => return self.submit(),
                _ => {}
            }
        }
        None
    }

    fn submit(&mut self) -> Option<Action> {
        if matches!(self.state, TdAuth::WaitRegistration) && !self.on_last {
            self.on_last = true;
            return None;
        }
        let value = if matches!(self.state, TdAuth::WaitPassword) {
            self.text.as_str()
        } else {
            self.text.trim()
        };
        let error = match self.state {
            TdAuth::WaitPhoneNumber
                if value
                    .chars()
                    .filter(|ch| ch.is_ascii_digit() || *ch == '+')
                    .count()
                    < 5 =>
            {
                Some("Include the country code, for example +1…")
            }
            TdAuth::WaitEmail if !value.contains('@') => Some("Enter an email address."),
            _ if value.is_empty() => Some("This field is required."),
            _ => None,
        };
        if let Some(error) = error {
            self.error = Some(error.into());
            return None;
        }
        let request = match self.state {
            TdAuth::WaitPhoneNumber => LoginRequest::Phone(normalize_phone(value)),
            TdAuth::WaitCode => LoginRequest::Code(value.into()),
            TdAuth::WaitPassword => LoginRequest::Password(value.into()),
            TdAuth::WaitEmail => LoginRequest::Email(value.into()),
            TdAuth::WaitEmailCode => LoginRequest::EmailCode(value.into()),
            TdAuth::WaitRegistration => LoginRequest::Registration {
                first: value.into(),
                last: self.last.trim().into(),
            },
            _ => return None,
        };
        self.busy = true;
        self.error = None;
        Some(Action::Login(request))
    }

    fn lines(&self) -> Vec<Line<'_>> {
        let mut lines = match self.state {
            TdAuth::Starting => vec![Line::from("Connecting to Telegram…"), hint("q quits")],
            TdAuth::WaitPhoneNumber if !self.phone_entry => {
                if self.busy {
                    vec![Line::from("Requesting a QR code…"), hint("q quits")]
                } else {
                    let mut lines = vec![Line::from("Choose how to sign in"), Line::default()];
                    for (selected, label) in [
                        (self.qr_selected, "Log in with a QR code"),
                        (!self.qr_selected, "Log in with a phone number"),
                    ] {
                        lines.push(Line::from(Span::styled(
                            format!(" {}  {label}", if selected { ">" } else { " " }),
                            if selected {
                                active_style()
                            } else {
                                Style::default()
                            },
                        )));
                    }
                    lines.extend([Line::default(), hint("↑↓ move    enter select    q quit")]);
                    lines
                }
            }
            TdAuth::WaitOtherDevice { .. } => vec![
                Line::from("Make the terminal larger to sign in."),
                hint("q quits"),
            ],
            TdAuth::Ready => Vec::new(),
            TdAuth::WaitPhoneNumber
            | TdAuth::WaitCode
            | TdAuth::WaitPassword
            | TdAuth::WaitEmail
            | TdAuth::WaitEmailCode
            | TdAuth::WaitRegistration => {
                let (title, label) = match self.state {
                    TdAuth::WaitPhoneNumber => ("Phone number", "Include the country code"),
                    TdAuth::WaitCode => ("Verification code", "Code from Telegram"),
                    TdAuth::WaitPassword => ("Cloud password", "Two-step verification"),
                    TdAuth::WaitEmail => ("Email address", "Telegram asked for an email"),
                    TdAuth::WaitEmailCode => ("Email code", "Code sent to your email"),
                    TdAuth::WaitRegistration => ("New account", "First name"),
                    _ => unreachable!(),
                };
                let registration = matches!(self.state, TdAuth::WaitRegistration);
                let mut lines = vec![Line::from(title), Line::default()];
                let fields = [
                    (label, &self.text, !self.on_last),
                    ("Last name", &self.last, self.on_last),
                ];
                for (label, value, selected) in
                    fields.into_iter().take(if registration { 2 } else { 1 })
                {
                    let shown = if matches!(self.state, TdAuth::WaitPassword) {
                        Cow::Owned("•".repeat(value.chars().count()))
                    } else {
                        Cow::Borrowed(value.as_str())
                    };
                    lines.push(Line::from(label));
                    lines.push(
                        Line::from(vec![
                            Span::raw(" "),
                            Span::raw(shown),
                            Span::raw(if selected { "█" } else { "" }),
                        ])
                        .style(if selected {
                            active_style()
                        } else {
                            Style::default()
                        }),
                    );
                }
                lines.push(Line::default());
                lines.push(hint(if registration {
                    "tab switches fields    enter continues    ctrl-c quits"
                } else if self.phone_entry {
                    "enter submits    esc back    ctrl-c quits"
                } else {
                    "enter submits    ctrl-c quits"
                }));
                lines
            }
        };
        lines.extend(self.error_lines());
        lines
    }

    fn error_lines(&self) -> impl Iterator<Item = Line<'_>> {
        self.error.as_deref().into_iter().flat_map(|error| {
            std::iter::once(Line::default())
                .chain(error.lines().map(|line| Line::styled(line, Color::Red)))
        })
    }

    fn card(&self, frame: &mut Frame<'_>, area: Rect, width: u16, height: u16) -> Rect {
        let card = centered_rect(width, height, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.app_context.style_border_component_focused())
            .title(if matches!(self.state, TdAuth::WaitOtherDevice { .. }) {
                " Sign in · q quits "
            } else {
                " Sign in "
            });
        let inner = block.inner(card);
        frame.render_widget(block, card);
        inner
    }

    fn draw_qr(&self, frame: &mut Frame<'_>, area: Rect) -> bool {
        let width = usize::from(area.width.saturating_sub(2));
        let height = usize::from(area.height.saturating_sub(2));
        let errors: Vec<_> = self.error_lines().collect();
        let error_width = errors.iter().map(Line::width).max().unwrap_or(0);
        if errors.len() > height || error_width > width {
            return false;
        }
        let Some((code, scale)) = self
            .qr
            .as_ref()
            .and_then(|qr| qr.pick(width, height - errors.len()))
        else {
            return false;
        };
        let pixels = (code.width() + QUIET * 2) * scale;
        let rows = pixels.div_ceil(2);
        let spare = height - errors.len() - rows;
        const INSTRUCTIONS: &str = "Settings → Devices → Link Desktop Device";
        let headers: &[&str] = if spare >= 2 && width >= INSTRUCTIONS.width() {
            &["Scan with Telegram on your phone", INSTRUCTIONS]
        } else if spare >= 1 {
            &["Scan with Telegram"]
        } else {
            &[]
        };
        // Fit was checked above; the centered code, including quiet zone, is entirely in bounds.
        let field = pixels
            .max(error_width)
            .max(usize::from(TEXT_W - 2).min(width));
        let inner = self.card(
            frame,
            area,
            (field + 2) as u16,
            (headers.len() + rows + errors.len() + 2) as u16,
        );
        for (row, header) in headers.iter().enumerate() {
            frame.render_widget(
                Paragraph::new(*header),
                Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
            );
        }
        let left = inner.x + ((field - pixels) / 2) as u16;
        let top = inner.y + headers.len() as u16;
        let buffer = frame.buffer_mut();
        let ink = active_style();
        for row in 0..rows {
            for col in 0..pixels {
                let glyph = match (
                    qr_dark(code, scale, col, row * 2),
                    qr_dark(code, scale, col, row * 2 + 1),
                ) {
                    (true, true) => "█",
                    (true, false) => "▀",
                    (false, true) => "▄",
                    (false, false) => " ",
                };
                buffer[(left + col as u16, top + row as u16)]
                    .set_symbol(glyph)
                    .set_style(ink);
            }
        }
        frame.render_widget(
            Paragraph::new(errors),
            Rect::new(
                inner.x,
                top + rows as u16,
                inner.width,
                inner.height - headers.len() as u16 - rows as u16,
            ),
        );
        true
    }
}

impl HandleFocus for LoginWindow {
    fn focus(&mut self) {}
    fn unfocus(&mut self) {}
}

impl Component for LoginWindow {
    fn handle_events(&mut self, event: Option<Event>) -> Result<Option<Action>, AppError<Action>> {
        Ok(match event {
            Some(Event::Key(code, modifiers)) => self.on_key(code, modifiers),
            Some(Event::Paste(text)) => {
                self.sync_auth();
                if !self.busy {
                    self.insert_str(&text);
                    self.app_context.mark_dirty();
                }
                None
            }
            _ => None,
        })
    }

    fn update(&mut self, action: Action) {
        self.sync_auth();
        if let Action::LoginFailed(error) = action {
            if !matches!(self.state, TdAuth::Ready) {
                self.error = Some(error);
                self.busy = false;
                self.app_context.mark_dirty();
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) -> io::Result<()> {
        self.sync_auth();
        if area.is_empty()
            || (matches!(self.state, TdAuth::WaitOtherDevice { .. }) && self.draw_qr(frame, area))
        {
            return Ok(());
        }
        let lines = self.lines();
        let width = TEXT_W.min(area.width);
        let inner_width = usize::from(width.saturating_sub(2).max(1));
        let height: usize = lines
            .iter()
            .map(|line| line.width().div_ceil(inner_width).max(1))
            .sum();
        let inner = self.card(
            frame,
            area,
            width,
            height.saturating_add(2).min(usize::from(area.height)) as u16,
        );
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        Ok(())
    }
}

fn active_style() -> Style {
    Style::default().fg(Color::Black).bg(Color::White)
}

fn hint(text: &'static str) -> Line<'static> {
    Line::styled(text, Style::default().add_modifier(Modifier::DIM))
}

fn qr_dark(code: &QrCode, scale: usize, x: usize, y: usize) -> bool {
    let mx = x / scale;
    let my = y / scale;
    mx >= QUIET
        && my >= QUIET
        && mx - QUIET < code.width()
        && my - QUIET < code.width()
        && code[(mx - QUIET, my - QUIET)] == QrColor::Dark
}

fn normalize_phone(raw: &str) -> String {
    let mut phone = String::with_capacity(raw.len());
    phone.extend(raw.chars().filter(|ch| ch.is_ascii_digit() || *ch == '+'));
    phone
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::{qr_dark, CachedQr, LoginWindow};
    use crate::{
        action::{Action, LoginRequest},
        components::{component_traits::Component, search_tests::create_test_app_context},
        event::Event,
        tg::login_phase::TdAuth,
    };
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    const LOGIN_LINK: &str = "tg://login?token=0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJK";

    #[test]
    fn qr_keeps_four_module_quiet_zone() {
        let cached = CachedQr::fresh(LOGIN_LINK);
        let (code, scale) = cached.pick(80, 22).expect("qr fits");
        let pixels = (code.width() + 8) * scale;
        assert!(qr_dark(code, scale, 4 * scale, 4 * scale));
        for x in 0..pixels {
            for y in 0..4 * scale {
                assert!(!qr_dark(code, scale, x, y));
                assert!(!qr_dark(code, scale, x, pixels - 1 - y));
            }
        }
        for y in 0..pixels {
            for x in 0..4 * scale {
                assert!(!qr_dark(code, scale, x, y));
                assert!(!qr_dark(code, scale, pixels - 1 - x, y));
            }
        }
    }

    #[test]
    fn qr_request_waits_for_response_and_can_retry_after_failure() {
        let context = create_test_app_context();
        *context.td_auth() = TdAuth::WaitPhoneNumber;
        let mut login = LoginWindow::new(context);
        assert!(matches!(
            login.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::Login(LoginRequest::Qr))
        ));
        assert!(login.on_key(KeyCode::Enter, KeyModifiers::NONE).is_none());
        login.update(Action::LoginFailed("Request rejected".into()));
        assert!(matches!(
            login.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::Login(LoginRequest::Qr))
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
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!text.contains("secret"));
        assert!(text.contains(&"•".repeat(password.chars().count())));
        assert!(matches!(
            login.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::Login(LoginRequest::Password(value))) if value == password
        ));
        *context.td_auth() = TdAuth::Ready;
        login.update(Action::Refresh);
        *context.td_auth() = TdAuth::WaitPassword;
        assert!(login.on_key(KeyCode::Enter, KeyModifiers::NONE).is_none());
    }
}
