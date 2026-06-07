use std::{
    cell::RefCell,
    collections::VecDeque,
    io::Cursor,
    process::Command,
    rc::Rc,
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

use glam::{Affine3A, Quat, Vec3, vec3};
use sysinfo::{Components, System};
use wgui::{
    assets::AssetPath,
    components::{
        ComponentTrait,
        bar_graph::{ComponentBarGraph, ValueCell},
        button::ComponentButton,
        slider::ComponentSlider,
    },
    drawing::Color,
    event::{CallbackDataCommon, EventListenerKind},
    i18n::Translation,
    parser::{Fetchable, ParseDocumentParams},
    renderer_vk::text::custom_glyph::CustomGlyphData,
    taffy::prelude::length,
    widget::{EventResult, image::WidgetImage, label::WidgetLabel, sprite::WidgetSprite},
};
use wlx_common::{
    common::LeftRight,
    windowing::{OverlayWindowState, Positioning},
};

use crate::{
    gui::{
        panel::{
            GuiPanel, NewGuiPanelParams, apply_custom_command, device_list::DeviceList,
            overlay_list::OverlayList, set_list::SetList,
        },
        timer::GuiTimer,
    },
    state::AppState,
    windowing::{Z_ORDER_WATCH, backend::OverlayEventData, window::OverlayWindowConfig},
};
use wayvr_ipc::packet_server::{PacketServer, WatchMediaCommand};

pub const WATCH_NAME: &str = "watch";

pub const WATCH_POS: Vec3 = vec3(-0.03, -0.01, 0.125);
pub const WATCH_ROT: Quat = Quat::from_xyzw(-0.707_106_6, 0.000_796_361_8, 0.707_106_6, 0.0);

const WATCH_GRAPH_LOW: Color = Color::new(0.63, 0.90, 0.57, 1.0);
const WATCH_GRAPH_WARN: Color = Color::new(0.96, 0.68, 0.22, 1.0);
const WATCH_GRAPH_HOT: Color = Color::new(0.93, 0.34, 0.22, 1.0);
struct WatchComponents {
    cpu_graph: Rc<ComponentBarGraph>,
    gpu_graph: Rc<ComponentBarGraph>,
    net_graph: Rc<ComponentBarGraph>,
    volume_slider: Rc<ComponentSlider>,
    power_buttons: [Rc<ComponentButton>; 3],
    media_buttons: [Rc<ComponentButton>; 2],
}

struct WatchIds {
    fps_avg: wgui::layout::WidgetID,
    fps_current: wgui::layout::WidgetID,
    notification: wgui::layout::WidgetID,
    media_title: wgui::layout::WidgetID,
    media_cover: wgui::layout::WidgetID,
    media_icon: wgui::layout::WidgetID,
    cpu_stat1: wgui::layout::WidgetID,
    cpu_stat2: wgui::layout::WidgetID,
    cpu_value: wgui::layout::WidgetID,
    gpu_stat1: wgui::layout::WidgetID,
    gpu_stat2: wgui::layout::WidgetID,
    gpu_value: wgui::layout::WidgetID,
    net_value: wgui::layout::WidgetID,
    core_max: wgui::layout::WidgetID,
    ram_value: wgui::layout::WidgetID,
    vram_value: wgui::layout::WidgetID,
    dropped_value: wgui::layout::WidgetID,
    power_value: wgui::layout::WidgetID,
    core_bars: Vec<wgui::layout::WidgetID>,
}

struct WatchLiveState {
    comps: WatchComponents,
    ids: WatchIds,
    system: System,
    components: Components,
    next_system_refresh: Instant,
    next_media_refresh: Instant,
    next_volume_refresh: Instant,
    next_fps_history_tick: Instant,
    pending_volume_percent: Rc<RefCell<Option<u32>>>,
    syncing_volume: Rc<RefCell<bool>>,
    cpu_count_seen: usize,
    gpu_count_seen: usize,
    net_count_seen: usize,
    fps_history: VecDeque<f32>, // one sample per second, 60 samples = 1 minute
    cover_placeholder: Option<CustomGlyphData>,
    media_play_glyph: Option<CustomGlyphData>,
    media_pause_glyph: Option<CustomGlyphData>,
    current_artwork: Option<String>,
    cover_rx: Option<Receiver<Option<Vec<u8>>>>,
    pending_power_idx: Rc<RefCell<Option<usize>>>,
    power_mode_watts: Rc<RefCell<[u32; 3]>>,
    amdgpu_hwmon: Option<std::path::PathBuf>,
    amdgpu_device: Option<std::path::PathBuf>,
}

#[derive(Default)]
struct WatchState {
    device_list: DeviceList,
    overlay_list: OverlayList,
    set_list: SetList,
    clock_12h: bool,
    live: Option<WatchLiveState>,
}

pub fn create_watch(app: &mut AppState) -> anyhow::Result<OverlayWindowConfig> {
    let state = WatchState {
        clock_12h: app.session.config.clock_12h,
        ..Default::default()
    };
    let watch_xml = "gui/watch-custom.xml";

    let mut panel =
        GuiPanel::new_from_template(app, watch_xml, state, NewGuiPanelParams::default())?;

    sets_or_overlays(&mut panel, app);
    init_live_state(&mut panel, app)?;

    let doc_params = ParseDocumentParams {
        globals: panel.layout.state.globals.clone(),
        path: AssetPath::FileOrBuiltIn(watch_xml),
        extra: panel.doc_extra.take().unwrap_or_default(),
    };

    panel.on_notify = Some(Box::new({
        let name = WATCH_NAME;
        move |panel, app, event_data| {
            let mut elems_changed = panel.state.overlay_list.on_notify(
                app,
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            elems_changed |= panel.state.set_list.on_notify(
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            elems_changed |= panel.state.device_list.on_notify(
                app,
                &mut panel.layout,
                &mut panel.parser_state,
                &event_data,
                &doc_params,
            )?;

            match event_data {
                OverlayEventData::EditModeChanged(edit_mode) => {
                    if let Ok(btn_edit_mode) = panel
                        .parser_state
                        .fetch_component_as::<ComponentButton>("btn_edit_mode")
                    {
                        btn_edit_mode.set_sticky_state(&mut panel.layout.common(), edit_mode);
                    }
                }
                OverlayEventData::SettingsChanged => {
                    panel.layout.mark_redraw();
                    sets_or_overlays(panel, app);

                    if app.session.config.clock_12h != panel.state.clock_12h {
                        panel.state.clock_12h = app.session.config.clock_12h;
                        if let Ok(clock_root) = panel.parser_state.get_widget_id("clock_root") {
                            panel.layout.remove_children(clock_root);

                            panel.parser_state.instantiate_template(
                                &doc_params,
                                "Clock",
                                &mut panel.layout,
                                clock_root,
                                Default::default(),
                            )?;

                            elems_changed = true;
                        }
                    }
                }
                OverlayEventData::CustomCommand { element, command } => {
                    if let Err(e) = apply_custom_command(panel, app, &element, &command) {
                        log::warn!("Could not apply {command:?} on {name}/{element}: {e:?}");
                    } else {
                        elems_changed = true;
                    }
                }
                _ => {}
            }

            if elems_changed {
                panel.process_custom_elems(app);
            }

            Ok(())
        }
    }));

    panel
        .timers
        .push(GuiTimer::new(Duration::from_millis(100), 0));

    #[cfg(feature = "openxr")]
    if let Some(monado) = &mut app.monado_state {
        let _ = monado.set_metrics_enabled(true);
    }

    let positioning = Positioning::FollowHand {
        hand: LeftRight::Left,
        lerp: 1.0,
    };

    panel.update_layout(app)?;

    Ok(OverlayWindowConfig {
        name: WATCH_NAME.into(),
        z_order: Z_ORDER_WATCH,
        default_state: OverlayWindowState {
            grabbable: false,
            interactable: true,
            positioning,
            transform: Affine3A::from_scale_rotation_translation(
                Vec3::ONE * 0.115,
                WATCH_ROT,
                WATCH_POS,
            ),
            angle_fade: true,
            ..OverlayWindowState::default()
        },
        show_on_spawn: app.session.config.enable_watch,
        global: true,
        ..OverlayWindowConfig::from_backend(Box::new(panel))
    })
}

fn init_live_state(panel: &mut GuiPanel<WatchState>, app: &mut AppState) -> anyhow::Result<()> {
    let comps = WatchComponents {
        cpu_graph: panel
            .parser_state
            .fetch_component_as::<ComponentBarGraph>("cpu_frametime_graph")?,
        gpu_graph: panel
            .parser_state
            .fetch_component_as::<ComponentBarGraph>("gpu_frametime_graph")?,
        net_graph: panel
            .parser_state
            .fetch_component_as::<ComponentBarGraph>("net_graph")?,
        volume_slider: panel
            .parser_state
            .fetch_component_as::<ComponentSlider>("volume_slider")?,
        power_buttons: [
            panel
                .parser_state
                .fetch_component_as::<ComponentButton>("btn_power_eco")?,
            panel
                .parser_state
                .fetch_component_as::<ComponentButton>("btn_power_balanced")?,
            panel
                .parser_state
                .fetch_component_as::<ComponentButton>("btn_power_max")?,
        ],
        media_buttons: [
            panel
                .parser_state
                .fetch_component_as::<ComponentButton>("btn_media_play_pause")?,
            panel
                .parser_state
                .fetch_component_as::<ComponentButton>("btn_media_next")?,
        ],
    };

    let ids = WatchIds {
        fps_avg: panel.parser_state.get_widget_id("fps_avg_label")?,
        fps_current: panel.parser_state.get_widget_id("fps_current_label")?,
        notification: panel.parser_state.get_widget_id("notification_label")?,
        media_title: panel.parser_state.get_widget_id("media_title_label")?,
        media_cover: panel.parser_state.get_widget_id("media_cover")?,
        media_icon: panel.parser_state.get_widget_id("media_icon")?,
        cpu_stat1: panel.parser_state.get_widget_id("cpu_stat1_label")?,
        cpu_stat2: panel.parser_state.get_widget_id("cpu_stat2_label")?,
        cpu_value: panel.parser_state.get_widget_id("cpu_value_label")?,
        gpu_stat1: panel.parser_state.get_widget_id("gpu_stat1_label")?,
        gpu_stat2: panel.parser_state.get_widget_id("gpu_stat2_label")?,
        gpu_value: panel.parser_state.get_widget_id("gpu_value_label")?,
        net_value: panel.parser_state.get_widget_id("net_value_label")?,
        core_max: panel.parser_state.get_widget_id("cpu_core_peak_label")?,
        ram_value: panel.parser_state.get_widget_id("ram_value_label")?,
        vram_value: panel.parser_state.get_widget_id("vram_value_label")?,
        dropped_value: panel.parser_state.get_widget_id("dropped_value_label")?,
        power_value: panel.parser_state.get_widget_id("power_value_label")?,
        core_bars: (0..32)
            .filter_map(|idx| {
                panel
                    .parser_state
                    .get_widget_id(&format!("core_{idx}"))
                    .ok()
            })
            .collect(),
    };

    let pending_volume_percent = Rc::new(RefCell::new(None));
    let syncing_volume = Rc::new(RefCell::new(false));
    comps.volume_slider.on_value_changed(Box::new({
        let pending_volume_percent = pending_volume_percent.clone();
        let syncing_volume = syncing_volume.clone();
        move |_common, event| {
            let percent = event.value.round().clamp(0.0, 100.0) as u32;
            if !*syncing_volume.borrow() {
                *pending_volume_percent.borrow_mut() = Some(percent);
            }
        }
    }));

    let power_mode_watts = Rc::new(RefCell::new([230u32, 304u32, 374u32]));
    let pending_power_idx: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
    let initial_power_idx = if let Some((current, max)) = lact_get_power_range() {
        power_mode_watts.borrow_mut()[2] = max;
        if current <= 230 {
            0
        } else if current >= max {
            2
        } else {
            1
        }
    } else {
        1
    };

    let power_buttons = comps.power_buttons.clone();
    for (idx, _button) in power_buttons.iter().enumerate() {
        let buttons = power_buttons.clone();
        let pending = pending_power_idx.clone();
        panel.add_event_listener(
            buttons[idx].base().get_id(),
            EventListenerKind::MousePress,
            Box::new(move |common, _, _, _| {
                for (button_idx, button) in buttons.iter().enumerate() {
                    button.set_sticky_state(common, button_idx == idx);
                }
                *pending.borrow_mut() = Some(idx);
                Ok(EventResult::Consumed)
            }),
        );
    }

    let media_buttons = comps.media_buttons.clone();
    for (idx, command) in [
        (0, WatchMediaCommand::PlayPause),
        (1, WatchMediaCommand::Next),
    ] {
        let buttons = media_buttons.clone();
        panel.add_event_listener(
            buttons[idx].base().get_id(),
            EventListenerKind::MousePress,
            Box::new(move |_common, _, app, _| {
                app.ipc_server.send_to_client(
                    wayvr_ipc::ipc::MEDIA_BRIDGE_CLIENT_NAME,
                    &PacketServer::WatchMediaCommand(command),
                );
                Ok(EventResult::Consumed)
            }),
        );
    }

    let root = panel.parser_state.get_widget_id("watch_panel")?;
    panel.add_event_listener(
        root,
        EventListenerKind::InternalStateChange,
        Box::new(move |common, _, app, state| {
            if let Some(live) = &mut state.live {
                live.refresh(common, app);
            }
            Ok(EventResult::Pass)
        }),
    );

    let mut system = System::new_all();
    system.refresh_cpu_usage();
    system.refresh_memory();
    let mut components = Components::new_with_refreshed_list();
    components.refresh(false);

    // The XML mounts a placeholder cover image; keep its glyph so we can restore
    // it whenever there's no artwork (or a download fails).
    let cover_placeholder = panel
        .layout
        .common()
        .state
        .widgets
        .get_as::<WidgetImage>(ids.media_cover)
        .and_then(|cover| cover.get_content());

    // Preload both media-control glyphs so the single `media_icon` sprite can be
    // swapped between play/pause at runtime (a button may only own one sprite).
    let media_play_glyph = CustomGlyphData::from_assets(
        &panel.layout.state.globals,
        AssetPath::BuiltIn("watch/media-play.svg"),
    )
    .ok();
    let media_pause_glyph = CustomGlyphData::from_assets(
        &panel.layout.state.globals,
        AssetPath::BuiltIn("watch/media-pause.svg"),
    )
    .ok();

    let mut live = WatchLiveState {
        comps,
        ids,
        system,
        components,
        next_system_refresh: Instant::now(),
        next_media_refresh: Instant::now(),
        next_volume_refresh: Instant::now(),
        next_fps_history_tick: Instant::now(),
        pending_volume_percent,
        syncing_volume,
        pending_power_idx,
        power_mode_watts,
        amdgpu_hwmon: find_amdgpu_hwmon(),
        amdgpu_device: find_amdgpu_device(),
        cpu_count_seen: 0,
        gpu_count_seen: 0,
        net_count_seen: 0,
        fps_history: VecDeque::new(),
        cover_placeholder,
        media_play_glyph,
        media_pause_glyph,
        current_artwork: None,
        cover_rx: None,
    };

    for (i, btn) in live.comps.power_buttons.iter().enumerate() {
        btn.set_sticky_state(&mut panel.layout.common(), i == initial_power_idx);
    }
    live.refresh(&mut panel.layout.common(), app);
    panel.state.live = Some(live);

    Ok(())
}

impl WatchLiveState {
    fn refresh(&mut self, common: &mut CallbackDataCommon, app: &mut AppState) {
        #[cfg(feature = "openxr")]
        if let Some(monado) = &mut app.monado_state {
            let _ = monado.set_metrics_enabled(true);
        }

        self.refresh_fps(common, app);
        self.refresh_notification(common, app);
        self.refresh_monado(common, app);

        let now = Instant::now();

        if now >= self.next_system_refresh {
            self.refresh_system(common, app);
            self.next_system_refresh = now + Duration::from_secs(1);
        }

        if now >= self.next_media_refresh {
            self.refresh_media(common, app);
            self.next_media_refresh = now + Duration::from_millis(100);
        }

        if now >= self.next_volume_refresh {
            self.refresh_volume(common);
            self.next_volume_refresh = now + Duration::from_millis(250);
        }
    }

    fn refresh_fps(&mut self, common: &mut CallbackDataCommon, app: &AppState) {
        #[cfg(feature = "openxr")]
        if let Some(monado) = &app.monado_state
            && let Some(fps_current) = monado.watch_metrics.fps_current
        {
            let now = Instant::now();
            if now >= self.next_fps_history_tick {
                if self.fps_history.len() >= 60 {
                    self.fps_history.pop_front();
                }
                self.fps_history.push_back(fps_current);
                self.next_fps_history_tick = now + Duration::from_secs(1);
            }
            let fps_avg = self.fps_history.iter().copied().sum::<f32>()
                / self.fps_history.len().max(1) as f32;
            set_label(common, self.ids.fps_current, &format!("{fps_current:.0}"));
            set_label(common, self.ids.fps_avg, &format!("AVG {fps_avg:.1}"));
            return;
        }

        set_label(
            common,
            self.ids.fps_current,
            &format!("{:.0}", app.watch_data.fps_current.max(0.0)),
        );
        set_label(
            common,
            self.ids.fps_avg,
            &format!("AVG {:.1}", app.watch_data.fps_average.max(0.0)),
        );
    }

    fn refresh_notification(&self, common: &mut CallbackDataCommon, app: &AppState) {
        let text = app
            .watch_data
            .latest_notification
            .as_ref()
            .map(|notification| {
                if notification.body.is_empty() {
                    notification.title.clone()
                } else if notification.title.is_empty() {
                    notification.body.clone()
                } else {
                    format!("{}   {}", notification.title, notification.body)
                }
            })
            .unwrap_or_default();

        set_label(common, self.ids.notification, &text);
    }

    fn refresh_monado(&mut self, common: &mut CallbackDataCommon, app: &AppState) {
        #[cfg(feature = "openxr")]
        if let Some(monado) = &app.monado_state {
            let fps_avg = if self.fps_history.is_empty() {
                monado.watch_metrics.fps_current
            } else {
                Some(self.fps_history.iter().copied().sum::<f32>() / self.fps_history.len() as f32)
            };
            // Scale by the compositor's frame budget, not by measured FPS:
            // deriving the scale from FPS makes the axis grow exactly when the
            // app slows down, so the bars shrink just as the frames get worse.
            let frame_budget_ms = monado
                .watch_metrics
                .display_period_ms
                .or_else(|| fps_avg.map(|fps| 1000.0 / fps.max(1.0)))
                .unwrap_or(12.0)
                .clamp(4.0, 40.0);
            let graph_limits = (0.0, frame_budget_ms * 2.0);
            self.comps.cpu_graph.set_limits(common, graph_limits);
            self.comps.gpu_graph.set_limits(common, graph_limits);

            self.cpu_count_seen = push_new_graph_values(
                &self.comps.cpu_graph,
                &monado.watch_metrics.cpu_ms,
                monado.watch_metrics.cpu_ms_count,
                self.cpu_count_seen,
                graph_limits,
            );
            self.gpu_count_seen = push_new_graph_values(
                &self.comps.gpu_graph,
                &monado.watch_metrics.gpu_ms,
                monado.watch_metrics.gpu_ms_count,
                self.gpu_count_seen,
                graph_limits,
            );
            self.net_count_seen = push_new_graph_values(
                &self.comps.net_graph,
                &monado.watch_metrics.net_ms,
                monado.watch_metrics.net_ms_count,
                self.net_count_seen,
                (0.0, 25.0),
            );

            set_label(
                common,
                self.ids.cpu_value,
                &format_ms(monado.watch_metrics.latest_cpu_ms),
            );
            set_label(
                common,
                self.ids.gpu_value,
                &format_ms(monado.watch_metrics.latest_gpu_ms),
            );

            set_label(
                common,
                self.ids.net_value,
                &format_ms(monado.watch_metrics.latest_net_ms),
            );
            set_label(
                common,
                self.ids.dropped_value,
                &monado.watch_metrics.dropped_frames.to_string(),
            );
            common.alterables.mark_redraw();
            return;
        }

        set_label(common, self.ids.cpu_value, "--");
        set_label(common, self.ids.gpu_value, "--");
        set_label(common, self.ids.net_value, "--");
        set_label(common, self.ids.dropped_value, "--");
    }

    fn refresh_system(&mut self, common: &mut CallbackDataCommon, app: &AppState) {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.components.refresh(false);

        let cpu_usage = self.system.global_cpu_usage();
        set_label(common, self.ids.cpu_stat1, &format!("{cpu_usage:.0} %"));
        set_label(
            common,
            self.ids.cpu_stat2,
            &format_temp(find_cpu_temp(&self.components)),
        );

        let gpu_usage = query_gpu_busy_sysfs(self.amdgpu_device.as_deref());
        set_label(
            common,
            self.ids.gpu_stat1,
            &gpu_usage.map_or_else(|| "-- %".into(), |busy| format!("{busy} %")),
        );
        set_label(
            common,
            self.ids.gpu_stat2,
            &format_temp(find_gpu_temp(&self.components)),
        );

        let used_gib = kib_to_gib(self.system.used_memory());
        let total_gib = kib_to_gib(self.system.total_memory());
        set_label(
            common,
            self.ids.ram_value,
            &format!("{used_gib:.1} / {total_gib:.1} GB"),
        );

        if let Some(idx) = self.pending_power_idx.borrow_mut().take() {
            let watts = self.power_mode_watts.borrow()[idx];
            lact_set_power_limit(watts);
        }

        let (draw, cap) = self
            .amdgpu_hwmon
            .as_deref()
            .and_then(query_gpu_power_sysfs)
            .unwrap_or_default();

        set_label(common, self.ids.power_value, &format!("{draw} W\n{cap} W"));

        set_label(
            common,
            self.ids.vram_value,
            &format_vram(query_vram_sysfs(self.amdgpu_device.as_deref())),
        );

        let buckets = cpu_usage_buckets(self.system.cpus(), self.ids.core_bars.len().max(1));
        let mut peak: f32 = 0.0;
        for (idx, widget_id) in self.ids.core_bars.iter().enumerate() {
            let usage = buckets.get(idx).copied().unwrap_or_default();
            peak = peak.max(usage);
            common.alterables.set_style(
                *widget_id,
                wgui::event::StyleSetRequest::Height(length(core_height(usage))),
            );
        }
        set_label(common, self.ids.core_max, &format!("{peak:.0} %"));

        if app.watch_data.latest_notification.is_none() {
            set_label(common, self.ids.notification, "");
        }
    }

    fn refresh_media(&mut self, common: &mut CallbackDataCommon, app: &AppState) {
        // The bridge pushes state ~once a second; if it goes quiet the source is
        // gone (tab closed, bridge disconnected), so fall back to "No media".
        let fresh = app
            .watch_data
            .media_updated
            .is_some_and(|updated| updated.elapsed() < Duration::from_secs(3));
        let media = fresh.then(|| app.watch_data.media.as_ref()).flatten();

        let text = match media {
            None => "No media".to_string(),
            Some(media) => match (&media.title, &media.artist) {
                (Some(title), Some(artist)) => format!("{title}\n{artist}"),
                (Some(title), None) => title.clone(),
                _ => "Nothing playing".to_string(),
            },
        };
        set_label(common, self.ids.media_title, &text);

        let artwork = media.and_then(|media| media.artwork.clone());
        self.refresh_cover(common, artwork);

        // Show the icon for the action the button performs: pause when playing,
        // play when paused.
        let playing = media.is_some_and(|media| media.playing);
        let glyph = if playing {
            self.media_pause_glyph.clone()
        } else {
            self.media_play_glyph.clone()
        };
        if let Some(mut sprite) = common
            .state
            .widgets
            .get_as::<WidgetSprite>(self.ids.media_icon)
        {
            sprite.set_content(common.alterables, glyph);
        }
    }

    /// Keep the cover sprite in sync with the current artwork URL. Downloading
    /// and decoding happens on a worker thread; the result is picked up here on
    /// a later refresh tick so the GUI thread never blocks on the network.
    fn refresh_cover(&mut self, common: &mut CallbackDataCommon, artwork: Option<String>) {
        if artwork != self.current_artwork {
            self.current_artwork = artwork.clone();
            match artwork {
                Some(url) => self.cover_rx = Some(spawn_cover_download(url)),
                None => {
                    self.cover_rx = None;
                    let placeholder = self.cover_placeholder.clone();
                    self.set_cover_glyph(common, placeholder);
                }
            }
        }

        match self.cover_rx.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => {
                self.cover_rx = None;
                let glyph = result
                    .and_then(|bytes| {
                        CustomGlyphData::from_bytes_raster(
                            &common.state.globals,
                            "watch_cover",
                            &bytes,
                        )
                        .ok()
                    })
                    .or_else(|| self.cover_placeholder.clone());
                self.set_cover_glyph(common, glyph);
            }
            Some(Err(TryRecvError::Disconnected)) => self.cover_rx = None,
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    fn set_cover_glyph(&self, common: &mut CallbackDataCommon, glyph: Option<CustomGlyphData>) {
        if let Some(mut cover) = common
            .state
            .widgets
            .get_as::<WidgetImage>(self.ids.media_cover)
        {
            cover.set_content(common.alterables, glyph);
        }
    }

    fn refresh_volume(&mut self, common: &mut CallbackDataCommon) {
        if let Some(percent) = self.pending_volume_percent.borrow_mut().take() {
            let _ = set_music_volume(percent);
            *self.syncing_volume.borrow_mut() = true;
            self.comps
                .volume_slider
                .set_value_primary(common, percent as f32);
            *self.syncing_volume.borrow_mut() = false;
            return;
        }

        if let Some(percent) = get_music_volume() {
            *self.syncing_volume.borrow_mut() = true;
            self.comps
                .volume_slider
                .set_value_primary(common, percent as f32);
            *self.syncing_volume.borrow_mut() = false;
        }
    }
}

fn sets_or_overlays(panel: &mut GuiPanel<WatchState>, app: &mut AppState) {
    let visible = if app.session.config.sets_on_watch {
        [false, true]
    } else {
        [true, false]
    };

    let widget = [
        panel
            .parser_state
            .get_widget_id("panels_root")
            .unwrap_or_default(),
        panel
            .parser_state
            .get_widget_id("sets_root")
            .unwrap_or_default(),
    ];

    for i in 0..2 {
        panel
            .layout
            .alterables
            .set_widget_visible(widget[i], visible[i]);
    }
}

fn push_new_graph_values(
    graph: &Rc<ComponentBarGraph>,
    values: &VecDeque<f32>,
    total_count: usize,
    previous_count: usize,
    limits: (f32, f32),
) -> usize {
    let new_count = total_count.saturating_sub(previous_count);
    if new_count == 0 {
        return total_count;
    }
    // Only values still in the queue can be pushed; older ones were dropped.
    let to_push = new_count.min(values.len());
    let skip = values.len().saturating_sub(to_push);
    for &value in values.iter().skip(skip) {
        // Sentinel -1 means a dropped frame: render as full-height red bar.
        let (bar_value, color) = if value < 0.0 {
            (limits.1, WATCH_GRAPH_HOT)
        } else {
            (value, graph_color(value, limits))
        };

        graph.push_value(ValueCell {
            value: bar_value,
            color,
        });
    }
    total_count
}

fn graph_color(value: f32, limits: (f32, f32)) -> Color {
    let midpoint = (limits.0 + limits.1) * 0.5;
    if value <= midpoint {
        return WATCH_GRAPH_LOW;
    }

    let t = ((value - midpoint) / (limits.1 - midpoint).max(0.001)).clamp(0.0, 1.0);
    if t < 0.6 {
        WATCH_GRAPH_LOW.lerp(&WATCH_GRAPH_WARN, t / 0.6)
    } else {
        WATCH_GRAPH_WARN.lerp(&WATCH_GRAPH_HOT, (t - 0.6) / 0.4)
    }
}

fn set_label(common: &mut CallbackDataCommon, widget_id: wgui::layout::WidgetID, text: &str) {
    if let Some(mut label) = common.state.widgets.get_as::<WidgetLabel>(widget_id) {
        label.set_text(common, Translation::from_raw_text(text));
    }
}

fn format_ms(value: Option<f32>) -> String {
    value.map_or_else(|| "--".into(), |value| format!("{value:.1} ms"))
}

fn format_temp(value: Option<f32>) -> String {
    value.map_or_else(|| "--".into(), |value| format!("{value:.0} C"))
}

fn kib_to_gib(value: u64) -> f32 {
    value as f32 / 1024.0 / 1024.0 / 1024.0
}

fn core_height(usage: f32) -> f32 {
    let min_height = 4.0;
    let max_height = 31.0;
    min_height + (usage.clamp(0.0, 100.0) / 100.0) * (max_height - min_height)
}

fn cpu_usage_buckets(cpus: &[sysinfo::Cpu], buckets: usize) -> Vec<f32> {
    if buckets == 0 {
        return Vec::new();
    }

    if cpus.is_empty() {
        return vec![0.0; buckets];
    }

    let mut out = vec![0.0; buckets];
    let mut counts = vec![0usize; buckets];

    for (idx, cpu) in cpus.iter().enumerate() {
        let bucket = idx * buckets / cpus.len();
        out[bucket] += cpu.cpu_usage();
        counts[bucket] += 1;
    }

    for (value, count) in out.iter_mut().zip(counts) {
        if count > 0 {
            *value /= count as f32;
        }
    }

    out
}

fn find_cpu_temp(components: &Components) -> Option<f32> {
    components.iter().find_map(|component| {
        let label = component.label().to_ascii_lowercase();
        if label.contains("package")
            || label.contains("tctl")
            || label.contains("tdie")
            || label.contains("cpu")
        {
            component.temperature().filter(|temp| temp.is_finite())
        } else {
            None
        }
    })
}

fn find_gpu_temp(components: &Components) -> Option<f32> {
    components.iter().find_map(|component| {
        let label = component.label().to_ascii_lowercase();
        if label.contains("gpu") || label.contains("edge") || label.contains("junction") {
            component.temperature().filter(|temp| temp.is_finite())
        } else {
            None
        }
    })
}

/// Name of the pipemeeter virtual input strip the watch slider controls.
const PIPEMEETER_MUSIC_STRIP: &str = "Music";

fn pipemeeter_socket_path() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("pipemeeter.sock")
}

/// Send one line-delimited JSON request to pipemeeter's control socket and
/// return the parsed response, or None if pipemeeter isn't running / errored.
fn pipemeeter_request(request: &serde_json::Value) -> Option<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(pipemeeter_socket_path()).ok()?;
    let timeout = Duration::from_millis(200);
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;

    let mut writer = stream.try_clone().ok()?;
    let mut line = serde_json::to_string(request).ok()?;
    line.push('\n');
    writer.write_all(line.as_bytes()).ok()?;
    writer.flush().ok()?;

    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).ok()?;
    serde_json::from_str(&response).ok()
}

/// Music strip volume as a 0..=100 percent, matching pipemeeter's 0.0..=1.0 slider value.
fn get_music_volume() -> Option<u32> {
    let response = pipemeeter_request(&serde_json::json!({
        "cmd": "get_volume",
        "strip": PIPEMEETER_MUSIC_STRIP,
    }))?;
    if !response.get("ok")?.as_bool()? {
        return None;
    }
    let volume = response.get("volume")?.as_f64()? as f32;
    Some((volume * 100.0).round().clamp(0.0, 100.0) as u32)
}

fn set_music_volume(percent: u32) -> bool {
    let volume = (percent as f32 / 100.0).clamp(0.0, 1.0);
    pipemeeter_request(&serde_json::json!({
        "cmd": "set_volume",
        "strip": PIPEMEETER_MUSIC_STRIP,
        "volume": volume,
    }))
    .and_then(|response| response.get("ok").and_then(|ok| ok.as_bool()))
    .unwrap_or(false)
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn format_vram(vram: Option<(Option<f32>, f32)>) -> String {
    match vram {
        Some((Some(used), total)) => format!("{used:.1} / {total:.1} GB"),
        Some((None, total)) => format!("-- / {total:.1} GB"),
        None => "--".into(),
    }
}

fn find_amdgpu_hwmon() -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let path = entry.path();
        let name = std::fs::read_to_string(path.join("name")).unwrap_or_default();
        if name.trim() == "amdgpu" && path.join("power1_cap").exists() {
            return Some(path);
        }
    }
    None
}

fn find_amdgpu_device() -> Option<std::path::PathBuf> {
    // Canonical hwmon path is …/device/hwmon/hwmonN; two levels up is the PCI device.
    let hwmon = find_amdgpu_hwmon()?;
    let canonical = hwmon.canonicalize().ok()?;
    let device = canonical.parent()?.parent()?;
    device
        .join("mem_info_vram_total")
        .exists()
        .then(|| device.to_path_buf())
}

fn query_gpu_power_sysfs(hwmon: &std::path::Path) -> Option<(u32, u32)> {
    let draw_uw: u64 = std::fs::read_to_string(hwmon.join("power1_average"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let cap_uw: u64 = std::fs::read_to_string(hwmon.join("power1_cap"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((draw_uw as u32 / 1_000_000, cap_uw as u32 / 1_000_000))
}

fn lact_get_power_range() -> Option<(u32, u32)> {
    // "Current power limit: 304W (Configurable Range: 212W to 374W)"
    let output = command_output("lact", &["cli", "power-limit", "get"])?;
    let current: u32 = output
        .split("Current power limit: ")
        .nth(1)?
        .split('W')
        .next()?
        .trim()
        .parse()
        .ok()?;
    let max: u32 = output
        .split(" to ")
        .nth(1)?
        .split('W')
        .next()?
        .trim()
        .parse()
        .ok()?;
    Some((current, max))
}

fn lact_set_power_limit(watts: u32) -> bool {
    Command::new("lact")
        .args(["cli", "power-limit", "set", &watts.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Total GPU utilization as a 0..=100 percent, read from amdgpu's
/// `gpu_busy_percent`. Mirrors the global CPU usage shown next to it.
fn query_gpu_busy_sysfs(device: Option<&std::path::Path>) -> Option<u32> {
    let busy: u32 = std::fs::read_to_string(device?.join("gpu_busy_percent"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(busy.min(100))
}

/// Fetch `url` over HTTP(S). Blocks on a smol runtime (upstream's `http_client`
/// runs the blocking `ureq` request on smol's global thread pool), so this must
/// run off the GUI thread (see [`spawn_cover_download`]).
fn download_cover_bytes(url: &str) -> Option<Vec<u8>> {
    smol::block_on(async {
        match dash_frontend::http_client::get_simple(url).await {
            Ok(response) => Some(response.data),
            Err(e) => {
                log::warn!("failed to download cover art: {e:?}");
                None
            }
        }
    })
}

/// Download the artwork at `url`, center-crop it to a square, scale to 128x128,
/// and return it as PNG bytes. Runs on a worker thread (see [`spawn_cover_download`]).
fn download_and_process_cover(url: &str) -> Option<Vec<u8>> {
    let bytes = download_cover_bytes(url)?;
    let image = image::load_from_memory(&bytes).ok()?;

    // Center-crop to a square, focusing on the middle of the artwork.
    let side = image.width().min(image.height());
    let x = (image.width() - side) / 2;
    let y = (image.height() - side) / 2;
    let square = image::imageops::crop_imm(&image, x, y, side, side).to_image();

    let scaled = image::imageops::resize(&square, 128, 128, image::imageops::FilterType::Lanczos3);

    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(scaled)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// Spawn a worker thread to fetch and process the cover art, returning a channel
/// that yields the processed PNG bytes (or `None` on failure) exactly once.
fn spawn_cover_download(url: String) -> Receiver<Option<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(download_and_process_cover(&url));
    });
    rx
}

fn query_vram_sysfs(device: Option<&std::path::Path>) -> Option<(Option<f32>, f32)> {
    let device = device?;
    let total_bytes: u64 = std::fs::read_to_string(device.join("mem_info_vram_total"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let used_bytes: u64 = std::fs::read_to_string(device.join("mem_info_vram_used"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let gib = 1024.0 * 1024.0 * 1024.0;
    Some((Some(used_bytes as f32 / gib), total_bytes as f32 / gib))
}
