//! Selection-aware inline image wrapper.
//!
//! Wraps a `gpui::img()` image element and checks the selection rectangle
//! during paint. When the image is selected, writes the alt text into the
//! shared `InlineState` so `Paragraph::selected_text()` can collect
//! shortcode text (e.g., `:hug:`).
//!
//! ## Selection model
//!
//! Unlike `Inline` (text) which performs per-character hit testing,
//! `InlineImage` uses an **all-or-nothing binary model**: if the image
//! bounds intersect the selection rectangle, the entire alt text is selected.
//!
//! ```text
//! Inline("Hello ")  InlineImage(🤗)  Inline(" World")
//!                   bounds ∩ selection?
//!                   → Yes: state.selection = 0..":hug:".len()
//!                   → No:  state.selection = None
//! ```

use std::sync::{Arc, Mutex};

use gpui::{
    px, quad, AnyElement, App, BorderStyle, Bounds, CursorStyle, Edges, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, Pixels,
    SharedString, Window,
};

use crate::{global_state::GlobalState, input::Selection, ActiveTheme};

use super::inline::InlineState;

/// A selection-aware inline image element.
///
/// Used in `Paragraph::render()` in place of bare `gpui::img()`, providing:
/// - Selection detection: marks the image as selected when its bounds overlap the selection rect
/// - Selection highlight: paints a translucent overlay on top of the image
/// - Alt text output: `selected_text()` returns the image's alt text (e.g., `:hug:`)
pub(super) struct InlineImage {
    id: ElementId,
    /// Alt text of the image, used as copy content when selected.
    alt_text: SharedString,
    /// The wrapped image child element (gpui::img() or a div-wrapped image).
    child: AnyElement,
    /// Shared state with InlineNode — selection written here is read by selected_text().
    state: Arc<Mutex<InlineState>>,
}

impl InlineImage {
    pub(super) fn new(
        id: impl Into<ElementId>,
        alt_text: SharedString,
        child: AnyElement,
        state: Arc<Mutex<InlineState>>,
    ) -> Self {
        Self {
            id: id.into(),
            alt_text,
            child,
            state,
        }
    }

    /// Check whether the image lies within the selection rectangle.
    /// Returns (is_selectable, selection).
    fn check_selection(
        &self,
        image_bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> (bool, Option<Selection>) {
        let Some(text_view_state) = GlobalState::global(cx).text_view_state() else {
            return (false, None);
        };

        let text_view_state = text_view_state.read(cx);
        let is_selectable = text_view_state.is_selectable();
        if !text_view_state.has_selection() {
            return (is_selectable, None);
        }

        let selection_bounds = text_view_state.selection_bounds();

        // Image bounds intersect selection rect → select entire alt text
        if image_bounds.intersects(&selection_bounds) {
            let alt_len = self.alt_text.len();
            if alt_len > 0 {
                (is_selectable, Some((0..alt_len).into()))
            } else {
                (is_selectable, None)
            }
        } else {
            (is_selectable, None)
        }
    }

    /// Paint a translucent selection highlight overlay on top of the image.
    fn paint_selection_overlay(
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_quad(quad(
            bounds,
            px(0.),
            cx.theme().selection,
            Edges::default(),
            gpui::transparent_black(),
            BorderStyle::default(),
        ));
    }
}

impl IntoElement for InlineImage {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InlineImage {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_element_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let layout_id = self.child.request_layout(window, cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.child.prepaint(window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let hitbox = prepaint;

        // 1. Paint the image itself
        self.child.paint(window, cx);

        // 2. Selection detection
        let (is_selectable, selection) = self.check_selection(bounds, window, cx);

        // 3. Update shared state (read by Paragraph::selected_text())
        {
            let mut state = self.state.lock().unwrap();
            state.selection = selection.clone();
        }

        // 4. Set cursor style
        if is_selectable || selection.is_some() {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }

        // 5. Paint selection highlight overlay
        if selection.is_some() {
            Self::paint_selection_overlay(bounds, window, cx);
        }
    }
}
