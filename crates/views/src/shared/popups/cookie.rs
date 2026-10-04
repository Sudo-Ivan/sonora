use std::rc::Rc;

use gpui::{App, Entity, IntoElement, RenderOnce, Window, div, prelude::*, px};
use i18n::{FluentArgs, Value as _, lookup, t};
use ui::{ActiveTheme as _, Button, Input, Modal, Text};

use crate::shared::steps::steps;

type Action = Rc<dyn Fn(&(), &mut Window, &mut App)>;

/// The walkthrough for the manual cookie sign-in: the page to send the user to, the site the
/// devtools open and the cookie to copy, plus the keys of every string the dialog shows.
const URL: &str = "https://www.deezer.com";
const SITE: &str = "www.deezer.com";
const COOKIE: &str = "arl";
const TITLE: &str = "login-cookie-named-title";
const HINT: &str = "login-cookie-named-hint";
const STEPS: [&str; 4] = [
    "login-cookie-named-step-1",
    "login-cookie-named-step-2",
    "login-cookie-named-step-3",
    "login-cookie-named-step-4",
];
const NOTE: &str = "login-cookie-named-note";

#[derive(IntoElement)]
pub(crate) struct CookiePrompt {
    provider: &'static str,
    secret: Entity<Input>,
    submit: Option<Action>,
    cancel: Option<Action>,
}

impl CookiePrompt {
    pub(crate) fn new(provider: &'static str, secret: Entity<Input>) -> Self {
        Self {
            provider,
            secret,
            submit: None,
            cancel: None,
        }
    }

    /// The hint key the paste field carries, set on the input when the manual sign-in starts
    /// so it follows the language like every other hint.
    pub(crate) fn hint() -> &'static str {
        HINT
    }

    pub(crate) fn on_submit(
        mut self,
        handler: impl Fn(&(), &mut Window, &mut App) + 'static,
    ) -> Self {
        self.submit = Some(Rc::new(handler));
        self
    }

    pub(crate) fn on_cancel(
        mut self,
        handler: impl Fn(&(), &mut Window, &mut App) + 'static,
    ) -> Self {
        self.cancel = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for CookiePrompt {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            provider,
            secret,
            submit,
            cancel,
            ..
        } = self;
        let dismissed = cancel.clone();
        let theme = *cx.theme();

        let mut args = FluentArgs::new();
        args.set("provider", provider.value());
        args.set("site", SITE.value());
        args.set("cookie", COOKIE.value());

        Modal::new("cookie-prompt", lookup(TITLE, Some(&args)))
            .w(px(560.))
            .child(
                Button::new("open-cookie-provider")
                    .label(lookup("login-cookie-open", Some(&args)))
                    .icon("icons/external-link.svg")
                    .outline()
                    .on_click(move |_, _, cx| cx.open_url(URL)),
            )
            .child(steps(STEPS.iter().map(|key| lookup(key, Some(&args)))))
            .child(
                div()
                    .child(lookup(NOTE, Some(&args)))
                    .flex_1()
                    .min_w_0()
                    .text_size(theme.text(Text::Small))
                    .text_color(theme.muted_foreground),
            )
            .child(secret)
            .action(
                Button::new("cancel-cookies")
                    .ghost()
                    .label(t!("common-cancel"))
                    .on_click(move |_, window, cx| {
                        if let Some(cancel) = &cancel {
                            cancel(&(), window, cx);
                        }
                    }),
            )
            .action(
                Button::new("submit-cookies")
                    .label(t!("login-cookie-submit"))
                    .primary()
                    .on_click(move |_, window, cx| {
                        if let Some(submit) = &submit {
                            submit(&(), window, cx);
                        }
                    }),
            )
            .on_dismiss(move |_, window, cx| {
                if let Some(dismissed) = &dismissed {
                    dismissed(&(), window, cx);
                }
            })
    }
}
