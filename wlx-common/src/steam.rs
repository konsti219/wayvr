use std::path::PathBuf;

use serde::Serialize;

pub type AppID = String;

pub fn get_steam_root() -> anyhow::Result<PathBuf> {
	let home = PathBuf::from(std::env::var("HOME")?);

	let steam_paths: [&str; 3] = [
		".steam/steam",
		".steam/debian-installation",
		".var/app/com.valvesoftware.Steam/data/Steam",
	];
	let Some(steam_path) = steam_paths.iter().map(|path| home.join(path)).find(|p| p.exists()) else {
		anyhow::bail!("Couldn't find Steam installation in search paths");
	};

	Ok(steam_path)
}

pub fn get_game_icon_path(app_id: &str) -> Option<String> {
	let steam_root = get_steam_root().ok()?;
	let librarycache = steam_root.join("appcache").join("librarycache");

	let candidates = [
		librarycache.join(format!("{app_id}_icon.jpg")),
		librarycache.join(format!("{app_id}_icon.png")),
		librarycache.join(format!("{app_id}_icon.ico")),
	];

	candidates
		.into_iter()
		.find(|path| path.exists())
		.map(|path| path.to_string_lossy().to_string())
}

#[derive(Serialize)]
pub struct RunningGame {
	pub app_id: String,
	pub pid: i32,
}

pub fn list_running_games() -> anyhow::Result<Vec<RunningGame>> {
	let mut res = Vec::<RunningGame>::new();

	let entries = std::fs::read_dir("/proc")?;
	for entry in entries.into_iter().flatten() {
		let path_cmdline = entry.path().join("cmdline");
		let Ok(cmdline) = std::fs::read(path_cmdline) else {
			continue;
		};

		let proc_file_name = entry.file_name();
		let Some(pid) = proc_file_name.to_str() else {
			continue;
		};

		let Ok(pid) = pid.parse::<i32>() else {
			continue;
		};

		let args: Vec<&str> = cmdline
			.split(|byte| *byte == 0x00)
			.filter_map(|arg| std::str::from_utf8(arg).ok())
			.collect();

		if !args.contains(&"SteamLaunch") {
			continue;
		}

		for arg in &args {
			let Some(app_id) = arg.strip_prefix("AppId=") else {
				continue;
			};

			let Ok(app_id_num) = app_id.parse::<u64>() else {
				continue;
			};

			res.push(RunningGame {
				app_id: app_id_num.to_string(),
				pid,
			});
			break;
		}
	}

	Ok(res)
}

pub fn launch(app_id: &AppID) -> anyhow::Result<()> {
	log::info!("Launching Steam game with AppID {}", app_id);
	call_steam(&format!("steam://rungameid/{}", app_id))?;
	Ok(())
}

fn call_steam(arg: &str) -> anyhow::Result<()> {
	match std::process::Command::new("xdg-open").arg(arg).spawn() {
		Ok(_) => Ok(()),
		Err(_) => {
			std::process::Command::new("steam").arg(arg).spawn()?;
			Ok(())
		}
	}
}

pub fn stop(app_id: &str, force_kill: bool) -> anyhow::Result<()> {
	log::info!("Stopping Steam game with AppID {}", app_id);
	for game in list_running_games()? {
		if game.app_id != app_id {
			continue;
		}

		log::info!(
			"Stopping Steam game with AppID {} and PID {}, force={}",
			app_id,
			game.pid,
			force_kill
		);

		let _ = std::process::Command::new("pkill")
			.arg(if force_kill { "-9" } else { "-15" })
			.arg("-P")
			.arg(format!("{}", game.pid))
			.spawn()?;
	}

	Ok(())
}
