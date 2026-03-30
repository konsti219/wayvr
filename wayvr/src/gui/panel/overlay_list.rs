use std::{collections::HashMap, rc::Rc};

use slotmap::{Key, SecondaryMap};
use wgui::{
    components::button::ComponentButton,
    event::{CallbackDataCommon, EventAlterables},
    layout::Layout,
    parser::{Fetchable, ParseDocumentParams, ParserState},
};
use wlx_common::steam;

use crate::windowing::{OverlayID, backend::OverlayEventData, window::OverlayCategory};
use crate::{state::AppState, windowing::backend::OverlayMeta};

#[derive(Default)]
/// Helper for managing a list of overlays
/// Populates `id="panels_root"` with `<Screen>`, `<Mirror>`, `<Panel>` templates
/// Populates `id="apps_root"` with `<App>` templates (optional)
/// Uses the following parameters: `name` (All), `display` (Screen, Mirror), `icon` (App, Panel)
pub struct OverlayList {
    overlay_buttons: SecondaryMap<OverlayID, Rc<ComponentButton>>,
    last_metas: Rc<[OverlayMeta]>,
    last_visible: Rc<[OverlayID]>,
}

impl OverlayList {
    pub fn on_notify(
        &mut self,
        app: &mut AppState,
        layout: &mut Layout,
        parser_state: &mut ParserState,
        event_data: &OverlayEventData,
        alterables: &mut EventAlterables,
        doc_params: &ParseDocumentParams,
    ) -> anyhow::Result<bool> {
        fn rebuild_pins_only(
            app: &mut AppState,
            layout: &mut Layout,
            parser_state: &mut ParserState,
            alterables: &mut EventAlterables,
            doc_params: &ParseDocumentParams,
        ) -> anyhow::Result<()> {
            let apps_root = parser_state.get_widget_id("apps_root").unwrap_or_default();
            if apps_root.is_null() {
                return Ok(());
            }

            layout.remove_children(apps_root);

            let stop_clicks = app.session.pinned_stop_clicks.clone();
            let mut stop_clicks_to_reset = Vec::<String>::new();

            for (i, pin) in app.session.config.pinned_apps.iter().enumerate() {
                let running = steam::list_running_games()
                    .unwrap_or_default()
                    .iter()
                    .any(|g| g.app_id == pin.app_id);

                if !running {
                    stop_clicks_to_reset.push(pin.app_id.clone());
                }

                let stop_stage = stop_clicks.get(&pin.app_id).copied().unwrap_or(0);

                let mut params = HashMap::new();
                params.insert("idx".into(), i.to_string().into());
                params.insert("name".into(), pin.name.as_str().into());
                params.insert(
                    "icon".into(),
                    if running && stop_stage >= 1 {
                        "".into()
                    } else {
                        pin.icon
                            .as_ref()
                            .map_or_else(|| "".into(), |x| x.as_str().into())
                    },
                );
                params.insert(
                    "icon_builtin".into(),
                    if running && stop_stage >= 2 {
                        "dashboard/knife.svg".into()
                    } else if running && stop_stage >= 1 {
                        "dashboard/remove_circle.svg".into()
                    } else {
                        "edit/panel.svg".into()
                    },
                );

                parser_state.instantiate_template(
                    doc_params,
                    "PinnedApp",
                    layout,
                    apps_root,
                    params,
                )?;

                let pin_button =
                    parser_state.fetch_component_as::<ComponentButton>(&format!("pinned_{i}"))?;
                if running {
                    let mut com = CallbackDataCommon {
                        alterables,
                        state: &layout.state,
                    };
                    pin_button.set_sticky_state(&mut com, true);
                }
            }

            for pin_id in stop_clicks_to_reset {
                app.session.pinned_stop_clicks.remove(&pin_id);
            }

            Ok(())
        }

        fn rebuild_buttons(
            app: &mut AppState,
            me: &mut OverlayList,
            layout: &mut Layout,
            parser_state: &mut ParserState,
            alterables: &mut EventAlterables,
            doc_params: &ParseDocumentParams,
        ) -> anyhow::Result<()> {
            let panels_root = parser_state
                .get_widget_id("panels_root")
                .unwrap_or_default();
            let apps_root = parser_state.get_widget_id("apps_root").unwrap_or_default();

            layout.remove_children(panels_root);
            me.overlay_buttons.clear();

            rebuild_pins_only(app, layout, parser_state, alterables, doc_params)?;

            for (i, meta) in me.last_metas.iter().enumerate() {
                let mut params = HashMap::new();

                let (template, root) = match meta.category {
                    OverlayCategory::Screen => {
                        params.insert(
                            "display".into(),
                            format!(
                                "{}{}",
                                (*meta.name).chars().next().unwrap_or_default(),
                                (*meta.name).chars().last().unwrap_or_default()
                            )
                            .into(),
                        );
                        ("Screen", panels_root)
                    }
                    OverlayCategory::Mirror => {
                        params.insert(
                            "display".into(),
                            (*meta.name).chars().last().unwrap().to_string().into(),
                        );
                        ("Mirror", panels_root)
                    }
                    OverlayCategory::Panel => {
                        let icon: Rc<str> = if let Some(icon) = meta.icon.as_ref() {
                            icon.to_string().into()
                        } else {
                            "edit/panel.svg".into()
                        };

                        params.insert("icon".into(), icon);
                        params.insert("icon_builtin".into(), "edit/panel.svg".into());
                        ("Panel", panels_root)
                    }
                    OverlayCategory::WayVR => {
                        params.insert(
                            "icon".into(),
                            meta.icon
                                .as_ref()
                                .expect("WayVR overlay without Icon attribute!")
                                .as_ref()
                                .into(),
                        );
                        params.insert("icon_builtin".into(), "edit/panel.svg".into());
                        ("App", apps_root)
                    }
                    OverlayCategory::Dashboard | OverlayCategory::Keyboard => {
                        let key = if matches!(meta.category, OverlayCategory::Dashboard) {
                            "btn_dashboard"
                        } else {
                            "btn_keyboard"
                        };

                        let Ok(overlay_button) =
                            parser_state.fetch_component_as::<ComponentButton>(key)
                        else {
                            continue;
                        };

                        if me.last_visible.as_ref().contains(&meta.id) {
                            let mut com = CallbackDataCommon {
                                alterables,
                                state: &layout.state,
                            };
                            overlay_button.set_sticky_state(&mut com, true);
                        }
                        me.overlay_buttons.insert(meta.id, overlay_button);
                        continue;
                    }
                    _ => continue,
                };

                if root.is_null() {
                    continue;
                }

                params.insert("idx".into(), i.to_string().into());
                params.insert("name".into(), meta.name.as_ref().into());
                parser_state.instantiate_template(doc_params, template, layout, root, params)?;
                let overlay_button =
                    parser_state.fetch_component_as::<ComponentButton>(&format!("overlay_{i}"))?;
                if me.last_visible.as_ref().contains(&meta.id) {
                    let mut com = CallbackDataCommon {
                        alterables,
                        state: &layout.state,
                    };
                    overlay_button.set_sticky_state(&mut com, true);
                }
                me.overlay_buttons.insert(meta.id, overlay_button);
            }

            Ok(())
        }

        let mut elements_changed = false;
        match event_data {
            OverlayEventData::OverlaysChanged(metas) => {
                self.last_metas = metas.clone();
                rebuild_buttons(app, self, layout, parser_state, alterables, doc_params)?;
                elements_changed = true;
            }
            OverlayEventData::SettingsChanged => {
                rebuild_pins_only(app, layout, parser_state, alterables, doc_params)?;
                elements_changed = true;
            }
            OverlayEventData::VisibleOverlaysChanged(overlays) => {
                self.last_visible = overlays.clone();

                let keyboard_id = self
                    .last_metas
                    .iter()
                    .find(|meta| matches!(meta.category, OverlayCategory::Keyboard))
                    .map(|meta| meta.id);
                if let Some(keyboard_id) = keyboard_id
                    && !overlays.as_ref().contains(&keyboard_id)
                {
                    // Closing the keyboard resets the stop click sequence.
                    app.session.pinned_stop_clicks.clear();
                }

                let mut com = CallbackDataCommon {
                    alterables,
                    state: &layout.state,
                };
                let mut overlay_buttons = self.overlay_buttons.clone();

                for visible in overlays.as_ref() {
                    if let Some(btn) = overlay_buttons.remove(*visible) {
                        btn.set_sticky_state(&mut com, true);
                    }
                }

                for btn in overlay_buttons.values() {
                    btn.set_sticky_state(&mut com, false);
                }
            }
            _ => {}
        }

        Ok(elements_changed)
    }
}
