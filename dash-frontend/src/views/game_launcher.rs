use std::rc::Rc;

use crate::{
	frontend::{FrontendTask, FrontendTasks, SoundType},
	util::{
		cached_fetcher::{self, CoverArt},
		pinning,
		steam_utils::{self, AppID, AppManifest},
	},
	views::game_cover,
};
use wgui::{
	assets::AssetPath,
	components::button::ComponentButton,
	globals::WguiGlobals,
	i18n::Translation,
	layout::{Layout, WidgetID},
	parser::{Fetchable, ParseDocumentParams, ParserState},
	task::Tasks,
	widget::{ConstructEssentials, label::WidgetLabel},
};
use wlx_common::async_executor::AsyncExecutor;
use wlx_common::dash_interface::BoxDashInterface;

#[derive(Clone)]
enum Task {
	FillAppDetails(cached_fetcher::AppDetailsJSONData),
	SetCoverArt(Rc<CoverArt>),
	TogglePinned,
	Launch,
}

pub struct Params<'a> {
	pub globals: &'a WguiGlobals,
	pub executor: AsyncExecutor,
	pub manifest: AppManifest,
	pub layout: &'a mut Layout,
	pub parent_id: WidgetID,
	pub frontend_tasks: &'a FrontendTasks,
	pub is_pinned: bool,
	pub on_launched: Box<dyn Fn()>,
}
pub struct View {
	#[allow(dead_code)]
	state: ParserState,
	tasks: Tasks<Task>,
	on_launched: Box<dyn Fn()>,
	frontend_tasks: FrontendTasks,

	game_cover_view_common: game_cover::ViewCommon,
	view_cover: game_cover::View,
	app_id: AppID,
	app_name: String,
	id_label_pin: WidgetID,
	pinned: bool,
}

impl View {
	async fn fetch_details(executor: AsyncExecutor, tasks: Tasks<Task>, app_id: AppID) {
		let Some(details) = cached_fetcher::get_app_details_json(executor, app_id).await else {
			return;
		};

		tasks.push(Task::FillAppDetails(details));
	}

	pub fn new(params: Params) -> anyhow::Result<Self> {
		let doc_params = &ParseDocumentParams {
			globals: params.globals.clone(),
			path: AssetPath::BuiltIn("gui/view/game_launcher.xml"),
			extra: Default::default(),
		};

		let state = wgui::parser::parse_from_assets(doc_params, params.layout, params.parent_id)?;

		{
			let mut label_title = state.fetch_widget_as::<WidgetLabel>(&params.layout.state, "label_title")?;
			label_title.set_text_simple(
				&mut params.globals.get(),
				Translation::from_raw_text(&params.manifest.name),
			);
		}

		let tasks = Tasks::new();

		// fetch details from the web
		let fut = View::fetch_details(params.executor.clone(), tasks.clone(), params.manifest.app_id.clone());
		params.executor.spawn(fut).detach();

		let id_cover_art_parent = state.get_widget_id("cover_art_parent")?;
		let btn_launch = state.fetch_component_as::<ComponentButton>("btn_launch")?;
		let btn_pin = state.fetch_component_as::<ComponentButton>("btn_pin")?;
		let id_label_pin = state.get_widget_id("label_pin")?;

		tasks.handle_button(&btn_launch, Task::Launch);
		tasks.handle_button(&btn_pin, Task::TogglePinned);

		let view_cover = game_cover::View::new(game_cover::Params {
			ess: &mut ConstructEssentials {
				layout: params.layout,
				parent: id_cover_art_parent,
			},
			executor: &params.executor,
			manifest: &params.manifest,
			on_loaded: {
				let tasks = tasks.clone();
				Box::new(move |cover_art| {
					tasks.push(Task::SetCoverArt(Rc::new(cover_art)));
				})
			},
			scale: 1.5,
		})?;

		let out = Self {
			state,
			tasks,
			on_launched: params.on_launched,
			frontend_tasks: params.frontend_tasks.clone(),
			game_cover_view_common: game_cover::ViewCommon::new(params.globals.clone()),
			view_cover,
			app_id: params.manifest.app_id.clone(),
			app_name: params.manifest.name,
			id_label_pin,
			pinned: params.is_pinned,
		};

		out.set_pin_button_label(params.layout)?;

		Ok(out)
	}

	pub fn update<T>(
		&mut self,
		layout: &mut Layout,
		interface: &mut BoxDashInterface<T>,
		data: &mut T,
	) -> anyhow::Result<()> {
		loop {
			let tasks = self.tasks.drain();
			if tasks.is_empty() {
				break;
			}
			for task in tasks {
				match task {
					Task::FillAppDetails(details) => self.action_fill_app_details(layout, details)?,
					Task::TogglePinned => self.action_toggle_pinned(layout, interface, data),
					Task::Launch => self.action_launch(),
					Task::SetCoverArt(cover_art) => {
						let _ = self
							.view_cover
							.set_cover_art(&mut self.game_cover_view_common, layout, &cover_art);
					}
				}
			}
		}

		Ok(())
	}

	fn set_pin_button_label(&self, layout: &mut Layout) -> anyhow::Result<()> {
		let mut c = layout.start_common();
		{
			let mut common = c.common();
			let mut label = common.state.widgets.cast_as::<WidgetLabel>(self.id_label_pin)?;
			let text = if self.pinned { "Unpin" } else { "Pin" };
			label.set_text(&mut common, Translation::from_raw_text(text));
		}
		c.finish()?;
		Ok(())
	}

	fn action_toggle_pinned<T>(&mut self, layout: &mut Layout, interface: &mut BoxDashInterface<T>, data: &mut T) {
		if self.pinned {
			let pins = interface.pinned_apps(data);
			if pinning::remove_pin_by_id(pins, &self.app_id) {
				self.pinned = false;
				interface.config_changed(data);
				self
					.frontend_tasks
					.push(FrontendTask::PushToast(Translation::from_raw_text(
						"Unpinned from keyboard",
					)));
			}
		} else {
			let manifest = AppManifest {
				app_id: self.app_id.clone(),
				run_game_id: self.app_id.clone(),
				name: self.app_name.clone(),
				raw_state_flags: 0,
				last_played: None,
			};
			let launch = pinning::build_game_pin(&manifest);
			if pinning::upsert_pin(interface.pinned_apps(data), launch) {
				self.pinned = true;
				interface.config_changed(data);
				self
					.frontend_tasks
					.push(FrontendTask::PushToast(Translation::from_raw_text(
						"Pinned to keyboard",
					)));
			}
		}

		let _ = self.set_pin_button_label(layout);
	}

	fn action_fill_app_details(
		&mut self,
		layout: &mut Layout,
		mut details: cached_fetcher::AppDetailsJSONData,
	) -> anyhow::Result<()> {
		let mut c = layout.start_common();

		{
			let label_author = self.state.fetch_widget(&c.layout.state, "label_author")?.widget;
			let label_description = self.state.fetch_widget(&c.layout.state, "label_description")?.widget;

			if let Some(developer) = details.developers.pop() {
				label_author
					.cast::<WidgetLabel>()?
					.set_text(&mut c.common(), Translation::from_raw_text_string(developer));
			}

			let desc = if let Some(desc) = &details.short_description {
				Some(desc)
			} else if let Some(desc) = &details.detailed_description {
				Some(desc)
			} else {
				None
			};

			if let Some(desc) = desc {
				label_description
					.cast::<WidgetLabel>()?
					.set_text(&mut c.common(), Translation::from_raw_text(desc));
			}
		}

		c.finish()?;
		Ok(())
	}

	fn action_launch(&mut self) {
		match steam_utils::launch(&self.app_id) {
			Ok(_) => {
				self
					.frontend_tasks
					.push(FrontendTask::PushToast(Translation::from_translation_key(
						"GAME_LAUNCHED",
					)));
				self.frontend_tasks.push(FrontendTask::PlaySound(SoundType::Launch));
			}
			Err(e) => {
				self
					.frontend_tasks
					.push(FrontendTask::PushToast(Translation::from_raw_text_string(format!(
						"Failed to launch: {:?}",
						e
					))));
			}
		}

		(*self.on_launched)();
	}
}
