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
    tg::login_phase::TdAuth,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc::UnboundedSender;

/// Main interface. Draws the sign-in card instead of the chat shell until
/// TDLib reports [`TdAuth::Ready`], and routes events to the focused side.
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

    pub fn update(&mut self, action: Action) {
        // The status bar also reads the area, so every component sees the action.
        self.login.update(action.clone());
        self.components
            .values_mut()
            .for_each(|component| component.update(action.clone()));
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) -> Result<(), AppError<()>> {
        if !matches!(*self.app_context.td_auth(), TdAuth::Ready) {
            self.login.draw(frame, area)?;
            return Ok(());
        }
        // The card stops drawing from here on; sync to Ready through the
        // regular update path so buffers and the QR cache are wiped.
        self.login.update(Action::Refresh);

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
