use crate::{FoxTapParams, FoxTapState};
use nih_plug::prelude::*;
use nih_plug_vizia::vizia::image as vimg;
use nih_plug_vizia::vizia::prelude::*;
use nih_plug_vizia::widgets::*;
use nih_plug_vizia::{assets, create_vizia_editor, ViziaState, ViziaTheming};
use std::sync::Arc;

const WINDOW_WIDTH: u32 = 400;
const WINDOW_HEIGHT: u32 = 550;

/// Background skin — embedded at compile time.
const BACKGROUND_PNG: &[u8] = include_bytes!("../assets/background.png");

#[derive(Lens)]
struct Data {
    params: Arc<FoxTapParams>,
    _foxtap_state: Arc<FoxTapState>,
}

impl Model for Data {}

pub(crate) fn default_state() -> Arc<ViziaState> {
    ViziaState::new(|| (WINDOW_WIDTH, WINDOW_HEIGHT))
}

pub(crate) fn create(
    params: Arc<FoxTapParams>,
    foxtap_state: Arc<FoxTapState>,
    editor_state: Arc<ViziaState>,
) -> Option<Box<dyn Editor>> {
    create_vizia_editor(editor_state, ViziaTheming::Custom, move |cx, _| {
        assets::register_noto_sans_light(cx);
        assets::register_noto_sans_thin(cx);

        cx.add_stylesheet(include_str!("../assets/style.css"))
            .expect("Failed to load stylesheet");

        // Load the background skin into vizia's resource manager
        if let Ok(img) = vimg::load_from_memory(BACKGROUND_PNG) {
            cx.load_image(
                "foxtap_bg",
                img,
                nih_plug_vizia::vizia::resource::ImageRetentionPolicy::Forever,
            );
        }

        Data {
            params: params.clone(),
            _foxtap_state: foxtap_state.clone(),
        }
        .build(cx);

        // All layers in a ZStack — background + absolutely positioned widgets
        ZStack::new(cx, |cx| {
            // Background image layer
            Element::new(cx)
                .class("bg-image");

            // Toggle button — absolutely positioned via CSS
            ParamButton::new(cx, Data::params, |params| &params.enabled)
                .class("toggle-btn");

            // Volume slider — absolutely positioned via CSS
            ParamSlider::new(cx, Data::params, |params| &params.stream_gain)
                .class("volume-slider");
        })
        .class("main-panel");
    })
}
