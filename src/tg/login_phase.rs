/// Authorization state needed by the sign-in screen.
#[derive(Clone, PartialEq, Eq, Default)]
pub enum TdAuth {
    /// Parameters have not been applied yet.
    #[default]
    Starting,
    /// Phone number or QR login can be requested.
    WaitPhoneNumber,
    /// A `tg://login?token=…` link is ready to show as a QR code.
    WaitOtherDevice { link: String },
    /// Telegram sent a login code.
    WaitCode,
    /// Cloud password (2FA).
    WaitPassword,
    /// Email address required.
    WaitEmail,
    /// Email verification code.
    WaitEmailCode,
    /// New account name.
    WaitRegistration,
    /// Session is authorized.
    Ready,
}

// Authorization links are credentials; AppContext's Debug must not expose them.
impl std::fmt::Debug for TdAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Starting => "Starting",
            Self::WaitPhoneNumber => "WaitPhoneNumber",
            Self::WaitOtherDevice { .. } => "WaitOtherDevice { link: [redacted] }",
            Self::WaitCode => "WaitCode",
            Self::WaitPassword => "WaitPassword",
            Self::WaitEmail => "WaitEmail",
            Self::WaitEmailCode => "WaitEmailCode",
            Self::WaitRegistration => "WaitRegistration",
            Self::Ready => "Ready",
        })
    }
}

impl TdAuth {
    /// True when the user has to do something before the chat shell can open.
    pub fn needs_user(&self) -> bool {
        !matches!(self, Self::Starting | Self::Ready)
    }
}
