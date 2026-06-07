use std::{path::PathBuf, rc::Rc};

use crate::{
	assets,
	testbed::{Testbed, TestbedUpdateParams},
};
use glam::Vec2;
use wgui::{
	assets::AssetPath,
	components::bar_graph::{ComponentBarGraph, ValueCell},
	drawing::Color,
	font_config::WguiFontConfig,
	globals::WguiGlobals,
	layout::{Layout, LayoutParams, LayoutUpdateParams},
	palette::WguiColorPalette,
	parser::{Fetchable, ParseDocumentParams, ParserState},
};
use wlx_common::locale::WayVRLangProvider;

pub struct TestbedAny {
	pub layout: Layout,
	#[allow(dead_code)]
	graphs: Vec<Rc<ComponentBarGraph>>,

	#[allow(dead_code)]
	state: ParserState,
}

impl TestbedAny {
	fn graph_color(value: f32, limits: (f32, f32)) -> Color {
		let low = Color::new(0.63, 0.90, 0.57, 1.0);
		let warn = Color::new(0.96, 0.68, 0.22, 1.0);
		let hot = Color::new(0.93, 0.34, 0.22, 1.0);

		let midpoint = (limits.0 + limits.1) * 0.5;
		if value <= midpoint {
			return low;
		}

		let t = ((value - midpoint) / (limits.1 - midpoint)).clamp(0.0, 1.0);
		if t < 0.6 {
			low.lerp(&warn, t / 0.6)
		} else {
			warn.lerp(&hot, (t - 0.6) / 0.4)
		}
	}

	fn push_samples(graph: &Rc<ComponentBarGraph>, values: &[f32], limits: (f32, f32)) {
		for value in values {
			graph.push_value(ValueCell {
				value: *value,
				color: Self::graph_color(*value, limits),
			});
		}
	}

	fn watch_custom_graphs(parser_state: &ParserState) -> anyhow::Result<Vec<Rc<ComponentBarGraph>>> {
		Ok(vec![
			parser_state.fetch_component_as::<ComponentBarGraph>("cpu_frametime_graph")?,
			parser_state.fetch_component_as::<ComponentBarGraph>("gpu_frametime_graph")?,
			parser_state.fetch_component_as::<ComponentBarGraph>("net_graph")?,
		])
	}

	fn init_watch_custom_graphs(graphs: &[Rc<ComponentBarGraph>]) {
		let cpu_samples = [
			4.1, 4.0, 3.9, 4.1, 4.2, 4.0, 4.3, 4.5, 4.2, 4.0, 3.8, 3.9, 4.0, 4.2, 4.4, 4.3, 4.1, 4.0,
			3.9, 4.0, 4.1, 4.3, 4.2, 4.1, 3.9, 4.0, 4.2, 4.4, 6.4, 7.1, 8.3, 6.8, 4.6, 4.3, 4.1, 4.0,
			4.2, 4.4, 4.3, 4.2, 4.1, 4.0, 4.1, 4.3, 6.7, 7.8, 9.1, 6.2,
		];
		let gpu_samples = [
			3.3, 3.4, 3.4, 3.5, 3.4, 3.3, 3.4, 3.5, 3.6, 3.5, 3.4, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8, 3.7,
			3.5, 3.4, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8, 4.1, 4.5, 6.2, 7.4, 8.8, 7.0, 4.8, 4.2, 3.9, 3.8,
			3.9, 4.1, 4.2, 4.4, 4.7, 5.0, 6.6, 7.2, 8.5, 9.4, 7.6, 5.1,
		];
		let net_samples = [
			7.0, 8.0, 8.5, 9.0, 8.0, 8.5, 9.0, 10.0, 9.5, 8.5, 8.0, 8.5, 9.0, 10.5, 11.5, 13.5, 15.5,
			17.5, 14.0, 11.0, 9.5, 8.5, 8.0, 8.5, 9.5, 11.0, 13.0, 16.5, 19.0, 15.0, 12.0, 10.0, 9.0,
			8.5, 8.0, 8.5, 9.0, 10.5, 12.5, 14.5, 17.0, 20.0, 16.0, 12.5, 10.0, 9.0, 8.5, 9.0,
		];

		Self::push_samples(&graphs[0], &cpu_samples, (0.0, 12.0));
		Self::push_samples(&graphs[1], &gpu_samples, (0.0, 12.0));
		Self::push_samples(&graphs[2], &net_samples, (0.0, 25.0));
	}

	pub fn new(assets: Box<assets::Asset>, name: &str) -> anyhow::Result<Self> {
		let path = if name.ends_with(".xml") {
			AssetPath::FileOrBuiltIn(name)
		} else {
			AssetPath::BuiltIn(&format!("gui/{name}.xml"))
		};

		let lang_provider = WayVRLangProvider::default();
		let palette_name = std::env::var("PALETTE").unwrap_or_else(|_| "Default".to_string());

		let globals = WguiGlobals::new(
			assets,
			&lang_provider,
			&WguiFontConfig::default(),
			PathBuf::new(), // cwd
			WguiColorPalette::get_builtin(&palette_name),
		)?;

		let (layout, state) = wgui::parser::new_layout_from_assets(
			&ParseDocumentParams {
				globals,
				path,
				extra: Default::default(),
			},
			LayoutParams::default(),
		)?;

		let graphs = if name == "watch-custom" {
			let graphs = Self::watch_custom_graphs(&state)?;
			Self::init_watch_custom_graphs(&graphs);
			graphs
		} else {
			Vec::new()
		};

		Ok(Self {
			layout,
			graphs,
			state,
		})
	}
}

impl Testbed for TestbedAny {
	fn update(&mut self, mut params: TestbedUpdateParams) -> anyhow::Result<()> {
		let res = self.layout.update(&mut LayoutUpdateParams {
			size: Vec2::new(params.width, params.height),
			timestep_alpha: params.timestep_alpha,
		})?;
		params.process_layout_result(res);
		Ok(())
	}

	fn layout(&mut self) -> &mut Layout {
		&mut self.layout
	}
}
