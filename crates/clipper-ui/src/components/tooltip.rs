//! Minimal text tooltip: `.tooltip(tooltip("Save replay"))` on any `Stateful` element.

use gpui::prelude::*;
use gpui::{AnyView, App, SharedString, Window, div, px};

use crate::theme::{ActiveTheme, Theme};

pub struct Tooltip {
    text: SharedString,
    theme: Theme,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let c = self.theme.colors;
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(c.border_strong)
            .bg(c.surface_raised)
            .text_xs()
            .text_color(c.text)
            .shadow_md()
            .max_w(px(280.))
            .child(self.text.clone())
    }
}

/// Builder for `StatefulInteractiveElement::tooltip`; snapshots the active theme on hover.
pub fn tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| {
        let theme = *cx.theme();
        let text = text.clone();
        cx.new(|_| Tooltip { text, theme }).into()
    }
}
