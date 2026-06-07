use std::path::{Path, PathBuf};

#[derive(rust_embed::Embed)]
#[folder = "assets/"]
pub struct Asset;

impl wgui::assets::AssetProvider for Asset {
	fn load_from_path(&mut self, path: &str) -> anyhow::Result<Vec<u8>> {
		if let Some(data) = Asset::get(path) {
			if path.starts_with("lang/") {
				return Ok(merge_lang_with_wayvr_fs(path, data.data.as_ref())?);
			}

			return Ok(data.data.to_vec());
		}

		load_from_wayvr_fs(path)
	}
}

fn merge_lang_with_wayvr_fs(path: &str, embedded: &[u8]) -> anyhow::Result<Vec<u8>> {
	let mut merged: serde_json::Value = serde_json::from_slice(embedded)?;

	if let Ok(wayvr) = load_from_wayvr_fs(path) {
		let wayvr: serde_json::Value = serde_json::from_slice(&wayvr)?;
		merge_json_objects(&mut merged, &wayvr);
	}

	Ok(serde_json::to_vec(&merged)?)
}

fn merge_json_objects(dst: &mut serde_json::Value, src: &serde_json::Value) {
	match (dst, src) {
		(serde_json::Value::Object(dst), serde_json::Value::Object(src)) => {
			for (key, value) in src {
				match dst.get_mut(key) {
					Some(existing) => merge_json_objects(existing, value),
					None => {
						dst.insert(key.clone(), value.clone());
					}
				}
			}
		}
		(dst, src) => {
			*dst = src.clone();
		}
	}
}

fn load_from_wayvr_fs(path: &str) -> anyhow::Result<Vec<u8>> {
	let full_path = wayvr_assets_root().join(path);
	std::fs::read(&full_path).map_err(|e| {
		anyhow::anyhow!(
			"embedded file {path} not found, and failed to read {}: {e}",
			full_path.display()
		)
	})
}

fn wayvr_assets_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.expect("uidev lives in the workspace root")
		.join("wayvr/src/assets")
}
