use crate::{
    action::Action,
    app_context::AppContext,
    app_error::AppError,
    component_name::ComponentName,
    components::{
        component_traits::Component, core_window::CoreWindow, login_window::LoginWindow,
        status_bar::StatusBar, title_bar::TitleBar, SMALL_AREA_HEIGHT, SMALL_AREA_WIDTH,
    },
    event::Event,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc::UnboundedSender;

/// Main interface. Draws the sign-in card instead of the chat shell until
/// the session is authorized, and routes events to the focused side.
pub struct Tui {
    app_context: Arc<AppContext>,
    components: HashMap<ComponentName, Box<dyn Component>>,
    login: LoginWindow,
}

impl Tui {
    pub fn new(app_context: Arc<AppContext>) -> Self {
        let components: [(ComponentName, Box<dyn Component>); 3] = [
            (
                ComponentName::TitleBar,
                TitleBar::new(Arc::clone(&app_context))
                    .with_name("Tgt")
                    .new_boxed(),
            ),
            (
                ComponentName::CoreWindow,
                CoreWindow::new(Arc::clone(&app_context))
                    .with_name("Core Window")
                    .new_boxed(),
            ),
            (
                ComponentName::StatusBar,
                StatusBar::new(Arc::clone(&app_context))
                    .with_name("Status Bar")
                    .new_boxed(),
            ),
        ];

        let login = LoginWindow::new(Arc::clone(&app_context));

        Tui {
            app_context,
            components: HashMap::from(components),
            login,
        }
    }

    pub fn register_action_handler(
        &mut self,
        tx: UnboundedSender<Action>,
    ) -> Result<(), AppError<Action>> {
        self.components
            .values_mut()
            .try_for_each(|component| component.register_action_handler(tx.clone()))?;
        Ok(())
    }

    pub fn handle_events(
        &mut self,
        event: Option<Event>,
    ) -> Result<Option<Action>, AppError<Action>> {
        if self.app_context.focused_component() == Some(ComponentName::Login) {
            return self.login.handle_events(event);
        }
        self.component(&ComponentName::CoreWindow)
            .handle_events(event)
    }

    /// The sign-in card owns authorization state, so credentials move into it
    /// without cloning; every other action fans out to the shell components.
    pub fn update(&mut self, action: Action) {
        match action {
            Action::Authorization(_) | Action::LoginFailed(_) => self.login.update(action),
            action => self
                .components
                .values_mut()
                .for_each(|component| component.update(action.clone())),
        }
    }

    /// True once TDLib reports the session authorized; the shell replaces the card.
    pub fn is_authorized(&self) -> bool {
        self.login.is_authorized()
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) -> Result<(), AppError<()>> {
        if !self.is_authorized() {
            self.login.draw(frame, area)?;
            return Ok(());
        }

        self.component(&ComponentName::StatusBar)
            .update(Action::UpdateArea(area));

        let core_window: &mut dyn std::any::Any =
            self.components.get_mut(&ComponentName::CoreWindow).unwrap();
        if let Some(core_window) = core_window.downcast_mut::<CoreWindow>() {
            core_window.with_small_area(area.width < SMALL_AREA_WIDTH);
        }

        let main_layout = Layout::new(
            Direction::Vertical,
            [
                Constraint::Length(if self.app_context.app_config().show_title_bar {
                    if area.height > SMALL_AREA_HEIGHT + 5 {
                        3
                    } else {
                        0
                    }
                } else {
                    0
                }),
                Constraint::Min(SMALL_AREA_HEIGHT),
                Constraint::Length(if self.app_context.app_config().show_status_bar {
                    if area.height > SMALL_AREA_HEIGHT + 5 {
                        4
                    } else {
                        0
                    }
                } else {
                    0
                }),
            ],
        )
        .split(area);

        self.component(&ComponentName::TitleBar)
            .draw(frame, main_layout[0])?;
        self.component(&ComponentName::CoreWindow)
            .draw(frame, main_layout[1])?;
        self.component(&ComponentName::StatusBar)
            .draw(frame, main_layout[2])?;

        Ok(())
    }

    fn component(&mut self, name: &ComponentName) -> &mut Box<dyn Component> {
        self.components.get_mut(name).unwrap_or_else(|| {
            tracing::error!("Failed to get component: {}", name);
            panic!("Failed to get component: {}", name)
        })
    }
}
