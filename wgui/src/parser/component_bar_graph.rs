use crate::{
	components::{Component, bar_graph},
	layout::WidgetID,
	parser::{AttribPair, ParserContext, process_component, style::parse_style},
	widget::ConstructEssentials,
};

pub fn parse_component_bar_graph(
	ctx: &mut ParserContext,
	parent_id: WidgetID,
	attribs: &[AttribPair],
	tag_name: &str,
) -> anyhow::Result<WidgetID> {
	let style = parse_style(ctx, attribs, tag_name);
	let mut limit_min = 0.0;
	let mut capacity = 50;
	let mut limit_max = 100.0;
	let mut unit = String::new();
	let mut show_limits = true;
	let mut show_midline = false;
	let mut bar_width = 0.0_f32;

	for pair in attribs {
		let (key, value) = (pair.attrib.as_ref(), pair.value.as_ref());
		#[allow(clippy::single_match)]
		match key {
			"capacity" => {
				ctx.parse_check_i32(tag_name, key, value, &mut capacity);
			}
			"limit_min" => {
				ctx.parse_check_f32(tag_name, key, value, &mut limit_min);
			}
			"limit_max" => {
				ctx.parse_check_f32(tag_name, key, value, &mut limit_max);
			}
			"unit" => {
				unit = value.to_string();
			}
			"show_limits" => {
				show_limits = matches!(value, "1" | "true");
			}
			"show_midline" => {
				show_midline = matches!(value, "1" | "true");
			}
			"bar_width" => {
				ctx.parse_check_f32(tag_name, key, value, &mut bar_width);
			}
			_ => {}
		}
	}

	let (widget, component) = bar_graph::construct(
		&mut ConstructEssentials {
			layout: ctx.layout,
			parent: parent_id,
		},
		bar_graph::Params {
			style,
			limits: (limit_min, limit_max),
			unit,
			capacity: capacity.try_into().unwrap_or(50),
			show_limits,
			show_midline,
			bar_width,
		},
	)?;

	process_component(ctx, Component(component), widget.id, attribs);

	Ok(widget.id)
}
