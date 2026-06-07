use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use glam::Vec2;
use taffy::{
	FlexDirection, JustifyContent,
	prelude::{auto, length, percent},
};

use crate::{
	components::{Component, ComponentBase, ComponentTrait, RefreshData},
	drawing::{self, GradientMode, PrimitiveExtent, RenderPrimitive},
	event::CallbackDataCommon,
	i18n::Translation,
	layout::{WidgetID, WidgetPair},
	renderer_vk::text::{FontWeight, HorizontalAlign, TextStyle},
	widget::{
		ConstructEssentials,
		custom_draw::{WidgetCustomDraw, WidgetCustomDrawParams},
		div::WidgetDiv,
		label::{WidgetLabel, WidgetLabelParams},
		rectangle::{WidgetRectangle, WidgetRectangleParams},
		util::WLength,
	},
};

#[derive(Default)]
pub struct Params {
	pub style: taffy::Style,
	pub limits: (f32, f32),
	pub unit: String,
	pub capacity: u32,
	pub show_limits: bool,
	pub show_midline: bool,
	/// Fixed bar width in pixels. 0 = auto (divide equally among all values).
	pub bar_width: f32,
}

pub struct ValueCell {
	pub value: f32,
	pub color: drawing::Color,
}

struct State {
	limits: (f32, f32), /* min - max */
	values: VecDeque<ValueCell>,
}

#[allow(clippy::struct_field_names)]
struct Data {
	#[allow(dead_code)]
	id_root: WidgetID,

	id_label_val_min: Option<WidgetID>,
	id_label_val_max: Option<WidgetID>,

	unit: String,
	capacity: u32,
}

pub struct ComponentBarGraph {
	base: ComponentBase,
	data: Rc<Data>,
	state: Rc<RefCell<State>>,
}

impl ComponentTrait for ComponentBarGraph {
	fn base(&self) -> &ComponentBase {
		&self.base
	}

	fn base_mut(&mut self) -> &mut ComponentBase {
		&mut self.base
	}

	fn refresh(&self, data: &mut RefreshData) {
		let state = self.state.borrow();
		self.update_limits_text(&state, &mut data.layout.common());
	}
}

impl ComponentBarGraph {
	fn update_limits_text(&self, state: &State, c: &mut CallbackDataCommon) -> Option<()> {
		let id_label_val_min = self.data.id_label_val_min?;
		let id_label_val_max = self.data.id_label_val_max?;

		let mut label_val_min = c.state.widgets.get_as::<WidgetLabel>(id_label_val_min)?;
		let mut label_val_max = c.state.widgets.get_as::<WidgetLabel>(id_label_val_max)?;

		label_val_min.set_text(
			c,
			Translation::from_raw_text_string(format!("{}{}", state.limits.0, self.data.unit)),
		);
		label_val_max.set_text(
			c,
			Translation::from_raw_text_string(format!("{}{}", state.limits.1, self.data.unit)),
		);

		Some(())
	}

	pub fn set_limits(&self, c: &mut CallbackDataCommon, limits: (f32, f32)) {
		let mut state = self.state.borrow_mut();
		state.limits = limits;
		self.update_limits_text(&state, c);
	}

	pub fn push_value(&self, cell: ValueCell) {
		let mut state = self.state.borrow_mut();
		if state.values.len() > self.data.capacity as usize {
			state.values.pop_front();
		}
		state.values.push_back(cell);
	}
}

pub fn construct(
	ess: &mut ConstructEssentials,
	mut params: Params,
) -> anyhow::Result<(WidgetPair, Rc<ComponentBarGraph>)> {
	const BORDER_COLOR: drawing::Color = drawing::Color::new(0.67, 0.67, 0.67, 0.5);
	const BG_COLOR: drawing::Color = drawing::Color::new(0.0, 0.0, 0.0, 0.6);
	let midline_color = BG_COLOR.lerp(&BORDER_COLOR, BORDER_COLOR.a);

	params.style.flex_direction = FlexDirection::Row;
	params.style.gap = length(if params.show_limits { 4.0 } else { 0.0 });

	// override style
	let (root, _) = ess.layout.add_child(ess.parent, WidgetDiv::create(), params.style)?;

	let vertical_texts = if params.show_limits {
		let (vertical_texts, _) = ess.layout.add_child(
			root.id,
			WidgetDiv::create(),
			taffy::Style {
				justify_content: Some(JustifyContent::SPACE_BETWEEN),
				flex_direction: FlexDirection::Column,
				size: taffy::Size {
					width: auto(),
					height: percent(1.0),
				},
				..Default::default()
			},
		)?;
		Some(vertical_texts)
	} else {
		None
	};

	let (rect, _) = ess.layout.add_child(
		root.id,
		WidgetRectangle::create(WidgetRectangleParams {
			border: 2.0,
			border_color: BORDER_COLOR.into(),
			round: WLength::Units(3.0),
			gradient: GradientMode::Vertical,
			color: BG_COLOR.into(),
			..Default::default()
		}),
		taffy::Style {
			position: taffy::Position::Relative,
			size: taffy::Size {
				width: percent(1.0),
				height: percent(1.0),
			},
			..Default::default()
		},
	)?;

	let state = Rc::new(RefCell::new(State {
		limits: params.limits,
		values: VecDeque::new(),
	}));

	let (_, _) = ess.layout.add_child(
		rect.id,
		WidgetCustomDraw::create(WidgetCustomDrawParams {
			func: {
				let state = state.clone();
				let show_midline = params.show_midline;
				let midline_color = midline_color;
				let bar_width = params.bar_width;
				Box::new(move |info| {
					let state = state.borrow();
					let (limit_min, limit_max) = state.limits;

					let box_width = info.boundary.width();
					let box_height = info.boundary.height();

					if show_midline {
						let line_height = 2.0;
						let line_y = (box_height * 0.5) - (line_height * 0.5);
						info.primitives.push(RenderPrimitive::Rectangle(
							PrimitiveExtent {
								boundary: drawing::Boundary {
									pos: Vec2::new(0.0, line_y),
									size: Vec2::new(box_width, line_height),
								},
								transform: info.transform.transform,
							},
							drawing::Rectangle {
								color: midline_color,
								..Default::default()
							},
						));
					}

					if state.values.is_empty() {
						return;
					}

					let (bar_width, skip) = if bar_width > 0.0 {
						let visible = (box_width / bar_width).floor() as usize;
						(bar_width, state.values.len().saturating_sub(visible))
					} else {
						(box_width / state.values.len() as f32, 0)
					};

					for (idx, cell) in state.values.iter().skip(skip).enumerate() {
						let norm_value = ((cell.value - limit_min) / (limit_max - limit_min)).clamp(0.0, 1.0);
						let bar_height = norm_value * box_height;
						// Snap bar edges to integer pixel boundaries so adjacent
						// 1px bars share the same edge rather than leaving gaps from
						// fractional container offsets.
						let bar_x = (bar_width * idx as f32).floor();
						let bar_end_x = (bar_width * (idx + 1) as f32).floor();
						let bar_y = box_height - bar_height;

						info.primitives.push(RenderPrimitive::Rectangle(
							PrimitiveExtent {
								boundary: drawing::Boundary {
									pos: Vec2::new(bar_x, bar_y),
									size: Vec2::new(bar_end_x - bar_x, bar_height),
								},
								transform: info.transform.transform,
							},
							drawing::Rectangle {
								color: cell.color,
								..Default::default()
							},
						));
					}
				})
			},
		}),
		taffy::Style {
			size: taffy::Size {
				width: percent(1.0),
				height: percent(1.0),
			},
			..Default::default()
		},
	)?;

	let label_params = WidgetLabelParams {
		style: TextStyle {
			align: Some(HorizontalAlign::Right),
			weight: Some(FontWeight::Bold),
			size: Some(11.0),
			..Default::default()
		},
		..Default::default()
	};

	let (id_label_val_max, id_label_val_min) = if let Some(vertical_texts) = vertical_texts {
		let label = WidgetLabel::create(&mut ess.layout.state, label_params.clone());
		let (label_val_max, _) = ess.layout.add_child(vertical_texts.id, label, Default::default())?;

		let label = WidgetLabel::create(&mut ess.layout.state, label_params);
		let (label_val_min, _) = ess.layout.add_child(vertical_texts.id, label, Default::default())?;

		(Some(label_val_max.id), Some(label_val_min.id))
	} else {
		(None, None)
	};

	let data = Rc::new(Data {
		id_root: root.id,
		id_label_val_min,
		id_label_val_max,
		unit: params.unit,
		capacity: params.capacity,
	});

	let base = ComponentBase {
		id: root.id,
		lhandles: Vec::new(),
	};

	let bar_graph = Rc::new(ComponentBarGraph { base, data, state });

	ess.layout.defer_component_refresh(Component(bar_graph.clone()));
	Ok((root, bar_graph))
}
