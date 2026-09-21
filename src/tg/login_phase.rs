use tdlib_rs::enums::AuthorizationState;

/// What TDLib last reported about authorization.
///
/// The login card does not store this. Keystrokes live on the widget. The link
/// sits on [`TdAuth::WaitOtherDevice`] because that is the update that carries it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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

impl TdAuth {
    /// Map a TDLib authorization update onto the subset the UI understands.
    ///
    /// States that need no screen (`WaitTdlibParameters`, closing, premium)
    /// return `None`. The caller applies those itself.
    pub fn from_authorization_state(state: &AuthorizationState) -> Option<Self> {
        match state {
            AuthorizationState::WaitPhoneNumber => Some(Self::WaitPhoneNumber),
            AuthorizationState::WaitOtherDeviceConfirmation(confirmation) => {
                Some(Self::WaitOtherDevice {
                    link: confirmation.link.clone(),
                })
            }
            AuthorizationState::WaitCode(_) => Some(Self::WaitCode),
            AuthorizationState::WaitPassword(_) => Some(Self::WaitPassword),
            AuthorizationState::WaitEmailAddress(_) => Some(Self::WaitEmail),
            AuthorizationState::WaitEmailCode(_) => Some(Self::WaitEmailCode),
            AuthorizationState::WaitRegistration(_) => Some(Self::WaitRegistration),
            AuthorizationState::Ready => Some(Self::Ready),
            _ => None,
        }
    }

    /// True when the user has to do something before the chat shell can open.
    pub fn needs_user(&self) -> bool {
        !matches!(self, Self::Starting | Self::Ready)
    }
}

#[cfg(test)]
mod tests {
    use super::TdAuth;
    use tdlib_rs::enums::AuthorizationState;
    use tdlib_rs::types::AuthorizationStateWaitOtherDeviceConfirmation;

    #[test]
    fn phone_wait_has_no_payload() {
        let auth = TdAuth::from_authorization_state(&AuthorizationState::WaitPhoneNumber);
        assert_eq!(auth, Some(TdAuth::WaitPhoneNumber));
    }

    #[test]
    fn other_device_keeps_the_link() {
        let state = AuthorizationState::WaitOtherDeviceConfirmation(
            AuthorizationStateWaitOtherDeviceConfirmation {
                link: "tg://login?token=abc".into(),
            },
        );
        assert_eq!(
            TdAuth::from_authorization_state(&state),
            Some(TdAuth::WaitOtherDevice {
                link: "tg://login?token=abc".into(),
            })
        );
    }

    #[test]
    fn ready_is_not_a_user_step() {
        let auth = TdAuth::from_authorization_state(&AuthorizationState::Ready).unwrap();
        assert!(!auth.needs_user());
    }
}
