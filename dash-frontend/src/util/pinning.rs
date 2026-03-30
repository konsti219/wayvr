use image::imageops::FilterType;
use wayvr_ipc::packet_client::WvrPinLaunchParams;
use wlx_common::cache_dir;

use crate::util::steam_utils::{self, AppManifest};

pub fn is_pinned(pinned: &[WvrPinLaunchParams], app_id: &str) -> bool {
	pinned.iter().any(|p| p.app_id == app_id)
}

pub fn upsert_pin(pinned: &mut Vec<WvrPinLaunchParams>, params: WvrPinLaunchParams) -> bool {
	if is_pinned(pinned, &params.app_id) {
		return false;
	}

	pinned.push(params);
	true
}

pub fn remove_pin_by_id(pinned: &mut Vec<WvrPinLaunchParams>, app_id: &str) -> bool {
	let old_len = pinned.len();
	pinned.retain(|p| p.app_id != app_id);
	old_len != pinned.len()
}

fn get_cached_cover_path(app_id: &str) -> std::path::PathBuf {
	cache_dir::get_path(&format!("cover_arts/{}.bin", app_id))
}

fn get_cropped_cover_icon_path(app_id: &str) -> Option<String> {
	let output_path = cache_dir::get_path(&format!("cover_arts/{}_square.png", app_id));
	if output_path.exists() {
		return Some(output_path.to_string_lossy().to_string());
	}

	let cover_data = std::fs::read(get_cached_cover_path(app_id)).ok()?;
	if cover_data.is_empty() {
		return None;
	}

	let cover = image::load_from_memory(&cover_data).ok()?;
	let width = cover.width();
	let height = cover.height();
	let side = width.min(height);
	if side == 0 {
		return None;
	}

	let x = (width - side) / 2;
	let y = (height - side) / 2;
	let square = cover
		.crop_imm(x, y, side, side)
		.resize_exact(256, 256, FilterType::Lanczos3);

	if let Some(parent) = output_path.parent() {
		if std::fs::create_dir_all(parent).is_err() {
			return None;
		}
	}

	if square.save_with_format(&output_path, image::ImageFormat::Png).is_err() {
		return None;
	}

	Some(output_path.to_string_lossy().to_string())
}

fn get_game_pin_icon_path(app_id: &str) -> Option<String> {
	steam_utils::get_game_icon_path(app_id).or_else(|| get_cropped_cover_icon_path(app_id))
}

pub fn build_game_pin(manifest: &AppManifest) -> WvrPinLaunchParams {
	WvrPinLaunchParams {
		name: manifest.name.clone(),
		app_id: manifest.run_game_id.clone(),
		icon: get_game_pin_icon_path(&manifest.app_id),
	}
}
