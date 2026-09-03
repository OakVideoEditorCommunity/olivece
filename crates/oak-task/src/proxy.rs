// Oak Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Oak Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! `ProxyTask`, mirroring `src/task/src/proxy/proxy.h`.
//!
//! Transcodes a video stream to a proxy via an external `ffmpeg` process
//! (oakcodec kind `OAKCODEC_TASK_PROXY`). The two pure functions
//! [`ProxyTask::build_arguments`] and [`ProxyTask::parse_progress`] are
//! parity/golden candidates (see tests/parity_test.rs).
//!
//! CPP-PARITY: src/task/src/proxy/proxy.h

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use oak_codec::proxymanager::ProxyManager;
use oak_codec::task::TaskRequest;

use crate::error::{Error, Result};
use crate::task::{Task, TaskBehavior};

/// The proxy parameters, mirroring `oakcodec_proxy_params` in
/// `include/codec/proxy.h` (kept as plain fields so no oakcodec handle is
/// needed to build arguments).
pub struct ProxyParams {
	/// Target width (0 = unspecified/divider-based).
	pub width: i32,
	/// Target height (0 = unspecified/divider-based).
	pub height: i32,
	/// Resolution divider.
	pub divider: i32,
	/// Proxy format version.
	pub version: i32,
	/// CRF for the proxy encode.
	pub crf: i32,
	/// Whether audio is included in the proxy.
	pub include_audio: bool,
	/// Output container extension.
	pub extension: String,
	/// ffmpeg preset name.
	pub preset: String,
}

/// A proxy transcode task: runs `ffmpeg` with arguments from
/// [`ProxyTask::build_arguments`], parsing progress lines via
/// [`ProxyTask::parse_progress`].
pub struct ProxyTask {
	/// The shared task base.
	pub base: Task,
	/// Source media filename.
	pub source_filename: String,
	/// Target stream index.
	pub stream_index: i32,
	/// Proxy parameters.
	pub params: ProxyParams,
	/// Output proxy filename.
	pub output_filename: String,
	/// Total media duration in seconds (for progress scaling).
	duration_seconds: f64,
}

/// 代理转码的编码器选择（按平台优先级探测到的第一个硬件编码器，
/// 否则软件 x264）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwEncoder {
	/// macOS VideoToolbox（`-c:v h264_videotoolbox`）。
	VideoToolbox,
	/// NVIDIA NVENC（Windows/Linux，`-c:v h264_nvenc`）。
	Nvenc,
	/// Intel QuickSync（`-c:v h264_qsv`）。
	Qsv,
	/// AMD AMF（`-c:v h264_amf`）。
	Amf,
	/// 软件 libx264（C++ 路径）。
	Software,
}

impl ProxyTask {
	/// Build a proxy task from an oakcodec request and proxy params,
	/// mirroring the C++ constructor (divider-based requests take the source
	/// fraction).
	pub fn new(request: &TaskRequest, params: ProxyParams) -> ProxyTask {
		let mut params = params;
		if request.proxy_width > 0 && request.proxy_height > 0 {
			params.width = request.proxy_width;
			params.height = request.proxy_height;
			params.divider = 1;
		}
		let title = format!(
			"Generating Proxy {}:{}",
			request.input_filename, request.stream_index
		);
		ProxyTask {
			base: Task::new(&title, None),
			source_filename: request.input_filename.to_string(),
			stream_index: request.stream_index,
			params,
			output_filename: request.output_filename.to_string(),
			duration_seconds: 0.0,
		}
	}

	/// Build the `ffmpeg` command-line argument vector for the proxy
	/// transcode. Mirrors `build_arguments` in proxy.cpp; the exact ordering
	/// and flag spelling are parity-locked (golden test).
	///
	/// CPP-PARITY: src/task/src/proxy/proxy.cpp (build_arguments)
	pub fn build_arguments(
		source_filename: &str,
		stream_index: i32,
		params: &ProxyParams,
		output_filename: &str,
	) -> Vec<String> {
		let scale_filter = if params.divider > 1 {
			format!(
				"scale=w=trunc(iw/{}/2)*2:h=trunc(ih/{}/2)*2",
				params.divider, params.divider
			)
		} else {
			format!(
				"scale=w={}:h={}:force_original_aspect_ratio=decrease",
				params.width, params.height
			)
		};

		let container_format = if params.extension.is_empty() {
			"mp4".to_string()
		} else {
			params.extension.clone()
		};

		let mut args: Vec<String> = vec![
			"-y".to_string(),
			"-nostats".to_string(),
			"-progress".to_string(),
			"pipe:1".to_string(),
			"-i".to_string(),
			source_filename.to_string(),
			"-map".to_string(),
			format!("0:{stream_index}"),
		];

		if params.include_audio {
			args.extend([
				"-map".to_string(),
				"0:a?".to_string(),
				"-c:a".to_string(),
				"aac".to_string(),
				"-b:a".to_string(),
				"128k".to_string(),
			]);
		} else {
			args.push("-an".to_string());
		}

		args.extend([
			"-vf".to_string(),
			scale_filter,
			"-c:v".to_string(),
			"libx264".to_string(),
			"-preset".to_string(),
			params.preset.clone(),
			"-crf".to_string(),
			params.crf.to_string(),
			"-pix_fmt".to_string(),
			"yuv420p".to_string(),
			"-movflags".to_string(),
			"+faststart".to_string(),
			"-f".to_string(),
			container_format,
			output_filename.to_string(),
		]);

		args
	}

	// ---- 硬件加速与资源控制 --------------------------------------------------
	//
	// build_arguments 是 C++ 纯软件路径（parity 锁定，不动）；实际转码走
	// [`ProxyTask::build_transcode_arguments`]：能用硬件编码器就一定用
	// （macOS VideoToolbox；Windows/Linux 的 NVENC/QSV/AMF；探测不到回退
	// libx264），解码侧 `-hwaccel auto`（HEVC 4:2:2 10-bit 在
	// Apple Silicon、RTX 50 系+、Intel 显卡上都能硬解；硬解失败 ffmpeg
	// 自动回退软解）。

	/// 探测本机可用的硬件 H.264 编码器（每进程每 ffmpeg 路径缓存一次：
	/// `ffmpeg -hide_banner -encoders` 的输出里按平台优先级找）。
	pub fn probe_hw_encoder(ffmpeg_path: &str) -> HwEncoder {
		static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, HwEncoder>>> =
			std::sync::OnceLock::new();
		let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
		let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
		if let Some(enc) = cache.get(ffmpeg_path) {
			return *enc;
		}
		let enc = Self::probe_hw_encoder_uncached(ffmpeg_path);
		cache.insert(ffmpeg_path.to_string(), enc);
		enc
	}

	fn probe_hw_encoder_uncached(ffmpeg_path: &str) -> HwEncoder {
		let out = Command::new(ffmpeg_path)
			.args(["-hide_banner", "-encoders"])
			.output();
		let Ok(out) = out else {
			return HwEncoder::Software;
		};
		let text = String::from_utf8_lossy(&out.stdout);
		let has = |name: &str| text.contains(name);
		// 平台优先级：macOS 只走 VideoToolbox；Windows NVENC → QSV →
		// AMF；Linux NVENC → QSV → AMF（VAAPI 需要 hwupload 帧上传链，
		// 暂不支持，回退软件）。
		let preference: &[(&str, HwEncoder)] = if cfg!(target_os = "macos") {
			&[("h264_videotoolbox", HwEncoder::VideoToolbox)]
		} else {
			&[
				("h264_nvenc", HwEncoder::Nvenc),
				("h264_qsv", HwEncoder::Qsv),
				("h264_amf", HwEncoder::Amf),
			]
		};
		for (name, enc) in preference {
			if has(name) {
				return *enc;
			}
		}
		HwEncoder::Software
	}

	/// libx264 预设名 → NVENC p1..p7（NVENC 预设是编号）。
	fn nvenc_preset(preset: &str) -> &'static str {
		match preset {
			"ultrafast" => "p1",
			"superfast" => "p2",
			"veryfast" => "p3",
			"faster" => "p4",
			"fast" => "p5",
			"medium" => "p6",
			_ => "p7",
		}
	}

	/// libx264 预设名 → AMF quality 档。
	fn amf_quality(preset: &str) -> &'static str {
		match preset {
			"ultrafast" | "superfast" | "veryfast" | "faster" | "fast" => "speed",
			"medium" => "balanced",
			_ => "quality",
		}
	}

	/// The video-codec arguments for `enc` (quality mapped from the
	/// proxy CRF: NVENC `-cq`、QSV `-global_quality`、AMF `-qp_i/-qp_p`
	/// 与 CRF 同刻度直接用；VideoToolbox 用 qscale 语义 `-q:v`）。
	fn hw_video_args(enc: HwEncoder, params: &ProxyParams) -> Vec<String> {
		match enc {
			HwEncoder::VideoToolbox => vec![
				"-c:v".to_string(),
				"h264_videotoolbox".to_string(),
				"-q:v".to_string(),
				params.crf.to_string(),
			],
			HwEncoder::Nvenc => vec![
				"-c:v".to_string(),
				"h264_nvenc".to_string(),
				"-preset".to_string(),
				Self::nvenc_preset(&params.preset).to_string(),
				"-cq".to_string(),
				params.crf.to_string(),
			],
			HwEncoder::Qsv => vec![
				"-c:v".to_string(),
				"h264_qsv".to_string(),
				"-global_quality".to_string(),
				params.crf.to_string(),
			],
			HwEncoder::Amf => vec![
				"-c:v".to_string(),
				"h264_amf".to_string(),
				"-quality".to_string(),
				Self::amf_quality(&params.preset).to_string(),
				"-rc".to_string(),
				"cqp".to_string(),
				"-qp_i".to_string(),
				params.crf.to_string(),
				"-qp_p".to_string(),
				params.crf.to_string(),
			],
			HwEncoder::Software => vec![
				"-c:v".to_string(),
				"libx264".to_string(),
				"-preset".to_string(),
				params.preset.clone(),
				"-crf".to_string(),
				params.crf.to_string(),
			],
		}
	}

	/// 实际转码用的参数：build_arguments 的软件路径 + 硬件加速与线程
	/// 控制（解码 `-hwaccel auto`；编码按探测结果；`-threads` 限制
	/// 解码/编码线程——后台任务不把机器打满）。
	pub fn build_transcode_arguments(
		source_filename: &str,
		stream_index: i32,
		params: &ProxyParams,
		output_filename: &str,
		enc: HwEncoder,
		threads: i32,
	) -> Vec<String> {
		// 软件路径直接复用 parity 参数，仅在最前面加解码 hwaccel 与
		// 线程数（不改变 C++ 的参数顺序语义之外的任何东西）。
		let base = Self::build_arguments(source_filename, stream_index, params, output_filename);
		let mut args: Vec<String> = vec!["-y".to_string(), "-nostats".to_string()];
		if threads > 0 {
			args.extend(["-threads".to_string(), threads.to_string()]);
		}
		args.extend(["-hwaccel".to_string(), "auto".to_string()]);
		// base 以 -y -nostats 开头，跳过这两个，余下的按序接上；编码
		// 器参数（-c:v 起）在 hw 时整段替换。
		let mut rest = base[2..].to_vec();
		if enc != HwEncoder::Software {
			// 找到 "-c:v libx264" 段（-c:v 起到 -pix_fmt 前），换成 hw 参数。
			if let Some(cv) = rest.iter().position(|a| a == "-c:v") {
				let end = rest[cv..]
					.iter()
					.position(|a| a == "-pix_fmt")
					.map(|p| cv + p)
					.unwrap_or(rest.len());
				rest.splice(cv..end, Self::hw_video_args(enc, params));
			}
		}
		args.append(&mut rest);
		args
	}

	/// 代理转码并发上限的配置键（Proxy Settings 对话框可调；
	/// 默认 1——后台任务不给系统造成压力）。
	pub const CONFIG_KEY_PROXY_MAX_CONCURRENT: &str = "ProxyMaxConcurrent";

	/// 配置的代理转码并发上限（[1, 16]）。
	pub fn proxy_max_concurrent() -> i32 {
		oak_core::configstore::ConfigStore::instance()
			.get_int(None, Self::CONFIG_KEY_PROXY_MAX_CONCURRENT, 1)
			.clamp(1, 16)
	}

	/// 后台转码的线程预算（不变式：并发数 × 每任务线程数 ≤
	/// 逻辑处理器数的一半——机器永远留一半以上给前台）。`concurrent`
	/// 是当前的并发上限配置。
	pub fn transcode_thread_budget(concurrent: i32) -> i32 {
		let cores = std::thread::available_parallelism()
			.map(|n| n.get())
			.unwrap_or(2) as i32;
		let half = (cores / 2).max(1);
		(half / concurrent.max(1)).clamp(1, 8)
	}

	/// Parse a single `ffmpeg -progress` output line into a progress value in
	/// 0.0..=1.0 given the total duration. Mirrors `parse_progress` in
	/// proxy.cpp (golden test).
	///
	/// CPP-PARITY: src/task/src/proxy/proxy.cpp (parse_progress)
	pub fn parse_progress(line: &str, duration_seconds: f64) -> Option<f64> {
		if duration_seconds <= 0.0 {
			return None;
		}

		let out_time_us: i64 = if let Some(rest) = line.strip_prefix("out_time_us=") {
			rest.trim().parse().unwrap_or(-1)
		} else if let Some(rest) = line.strip_prefix("out_time_ms=") {
			// Despite the name, ffmpeg reports this value in microseconds.
			rest.trim().parse().unwrap_or(-1)
		} else {
			-1
		};

		if out_time_us < 0 {
			return None;
		}

		let progress = out_time_us as f64 / 1_000_000.0 / duration_seconds;
		Some(if progress < 0.0 {
			0.0
		} else if progress > 1.0 {
			1.0
		} else {
			progress
		})
	}
}

impl TaskBehavior for ProxyTask {
	/// Spawn `ffmpeg` with the built arguments, feed `-progress` lines to
	/// [`ProxyTask::parse_progress`] and emit them as task progress.
	fn run(&mut self, task: &mut Task) -> Result<()> {
		// Direct call into oakcodec's proxy manager (single-lib
		// unification: the old two-stage C ABI getter is gone; an empty
		// string means "not found"). The `FFmpegPath` config takes
		// precedence when set (C++ `OAK_CONFIG("FFmpegPath")`).
		let configured = oak_core::configstore::ConfigStore::instance()
			.get(None, "FFmpegPath")
			.unwrap_or_default();
		let ffmpeg_path = ProxyManager::find_ffmpeg(&configured);
		if ffmpeg_path.is_empty() {
			task.set_error(
				"Failed to generate proxy: ffmpeg executable was not found. Set the ffmpeg path in Preferences > Disk > Proxy Settings.",
			);
			return Err(Error::Failed("ffmpeg executable was not found".to_string()));
		}

		// Create the output directory if needed.
		if let Some(parent) = std::path::Path::new(&self.output_filename).parent() {
			if !parent.as_os_str().is_empty() && !parent.exists() {
				if std::fs::create_dir_all(parent).is_err() {
					task.set_error("Failed to create proxy output directory");
					return Err(Error::Failed(
						"Failed to create proxy output directory".to_string(),
					));
				}
			}
		}

		let _ = std::fs::remove_file(&self.output_filename);
		let working_filename = format!("{}.working.mp4", self.output_filename);
		let _ = std::fs::remove_file(&working_filename);

		// 硬件加速（能用硬编就一定用；探测不到回退 libx264）+ 线程
		// 预算（后台任务不把机器打满）。
		let encoder = Self::probe_hw_encoder(&ffmpeg_path);
		let threads = Self::transcode_thread_budget(Self::proxy_max_concurrent());
		let args = Self::build_transcode_arguments(
			&self.source_filename,
			self.stream_index,
			&self.params,
			&working_filename,
			encoder,
			threads,
		);

		// Probe the source duration for progress scaling (0 when unknown).
		self.duration_seconds = probe_source_duration_seconds(&ffmpeg_path, &self.source_filename);

		// Unix 上降优先级跑（nice 10：代理是后台任务，不能和前台
		// 抢系统）；Windows 没有 nice，线程预算已限制占用。
		#[cfg(unix)]
		let mut command = {
			let mut c = Command::new("nice");
			c.arg("-n").arg("10").arg(&ffmpeg_path);
			c
		};
		#[cfg(not(unix))]
		let mut command = Command::new(&ffmpeg_path);
		command
			.args(&args)
			.stdout(Stdio::piped())
			.stderr(Stdio::piped());
		let mut child = match command.spawn() {
			Ok(child) => child,
			Err(_) => {
				task.set_error("Failed to start ffmpeg for proxy generation");
				return Err(Error::Failed("Failed to start ffmpeg".to_string()));
			}
		};

		// Drain stderr in a thread so a chatty ffmpeg cannot block us.
		let stderr = child.stderr.take();
		if let Some(stderr) = stderr {
			std::thread::spawn(move || {
				let _ = BufReader::new(stderr).lines().count();
			});
		}

		let stdout = child.stdout.take();
		let mut last_progress = 0.0_f64;
		if let Some(stdout) = stdout {
			let reader = BufReader::new(stdout);
			for line in reader.lines() {
				match line {
					Ok(line) => {
						if let Some(progress) = Self::parse_progress(&line, self.duration_seconds) {
							if progress >= 0.0 && progress - last_progress > 0.001 {
								last_progress = progress;
								task.emit_progress(progress);
							}
						}
					}
					Err(_) => break,
				}
				if task.is_cancelled() {
					let _ = child.kill();
					let _ = child.wait();
					let _ = std::fs::remove_file(&working_filename);
					task.set_error("Proxy generation was cancelled");
					return Err(Error::Cancelled);
				}
			}
		}

		let exit_status = child.wait();
		if !matches!(exit_status, Ok(status) if status.success()) {
			let _ = std::fs::remove_file(&working_filename);
			task.set_error("ffmpeg failed to generate proxy");
			return Err(Error::Failed("ffmpeg failed to generate proxy".to_string()));
		}

		if !std::path::Path::new(&working_filename).exists() {
			task.set_error("ffmpeg finished but proxy file was not created");
			return Err(Error::Failed(
				"ffmpeg finished but proxy file was not created".to_string(),
			));
		}

		if std::fs::rename(&working_filename, &self.output_filename).is_err() {
			task.set_error("Failed to move proxy into place");
			return Err(Error::Failed("Failed to move proxy into place".to_string()));
		}

		task.emit_progress(1.0);
		Ok(())
	}
}

/// Probe the source duration via `ffprobe` next to ffmpeg; 0.0 when
/// unavailable (in which case no intermediate progress is reported).
fn probe_source_duration_seconds(ffmpeg_path: &str, source_filename: &str) -> f64 {
	let ffprobe = std::path::Path::new(ffmpeg_path).with_file_name("ffprobe");
	if !ffprobe.exists() {
		return 0.0;
	}
	let output = Command::new(&ffprobe)
		.args([
			"-v",
			"error",
			"-show_entries",
			"format=duration",
			"-of",
			"default=noprint_wrappers=1:nokey=1",
			source_filename,
		])
		.output();
	match output {
		Ok(out) if out.status.success() => {
			let text = String::from_utf8_lossy(&out.stdout);
			text.trim().parse().unwrap_or(0.0)
		}
		_ => 0.0,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn params() -> ProxyParams {
		ProxyParams {
			width: 1280,
			height: 720,
			divider: 1,
			version: 1,
			crf: 23,
			include_audio: true,
			extension: "mp4".to_string(),
			preset: "veryfast".to_string(),
		}
	}

	/// 软件路径 = parity 参数 + 解码 hwaccel/线程前缀；编码器参数
	/// 原样（libx264 + preset + crf）。
	#[test]
	fn transcode_arguments_software_keeps_parity_body() {
		let args = ProxyTask::build_transcode_arguments(
			"/src.mov", 0, &params(), "/dst.mp4", HwEncoder::Software, 4,
		);
		assert!(args.windows(2).any(|w| w == ["-hwaccel", "auto"]));
		assert!(args.windows(2).any(|w| w == ["-threads", "4"]));
		assert!(args.windows(2).any(|w| w == ["-c:v", "libx264"]));
		assert!(args.windows(2).any(|w| w == ["-crf", "23"]));
		// -threads/-hwaccel 在 -i 之前（解码侧选项）。
		let i_pos = args.iter().position(|a| a == "-i").unwrap();
		let hw_pos = args.iter().position(|a| a == "-hwaccel").unwrap();
		assert!(hw_pos < i_pos);
	}

	/// 硬件编码器整段替换 -c:v 段（不再出现 libx264/crf），各自的
	/// 质量/预设映射正确。
	#[test]
	fn transcode_arguments_hw_swaps_encoder() {
		let vt = ProxyTask::build_transcode_arguments(
			"/src.mov", 0, &params(), "/dst.mp4", HwEncoder::VideoToolbox, 4,
		);
		assert!(vt.windows(2).any(|w| w == ["-c:v", "h264_videotoolbox"]));
		assert!(vt.windows(2).any(|w| w == ["-q:v", "23"]));
		assert!(!vt.iter().any(|a| a == "libx264" || a == "-crf"));

		let nv = ProxyTask::build_transcode_arguments(
			"/src.mov", 0, &params(), "/dst.mp4", HwEncoder::Nvenc, 4,
		);
		assert!(nv.windows(2).any(|w| w == ["-c:v", "h264_nvenc"]));
		assert!(nv.windows(2).any(|w| w == ["-preset", "p3"]));
		assert!(nv.windows(2).any(|w| w == ["-cq", "23"]));

		let qsv = ProxyTask::build_transcode_arguments(
			"/src.mov", 0, &params(), "/dst.mp4", HwEncoder::Qsv, 4,
		);
		assert!(qsv.windows(2).any(|w| w == ["-c:v", "h264_qsv"]));
		assert!(qsv.windows(2).any(|w| w == ["-global_quality", "23"]));

		let amf = ProxyTask::build_transcode_arguments(
			"/src.mov", 0, &params(), "/dst.mp4", HwEncoder::Amf, 4,
		);
		assert!(amf.windows(2).any(|w| w == ["-c:v", "h264_amf"]));
		assert!(amf.windows(2).any(|w| w == ["-quality", "speed"]));
		// -pix_fmt/-movflags 等后续段在替换后仍然保留。
		assert!(vt.windows(2).any(|w| w == ["-pix_fmt", "yuv420p"]));
	}

	/// 线程预算不变式：并发 × 线程 ≤ 逻辑处理器数一半；区间 [1, 8]。
	#[test]
	fn transcode_thread_budget_invariant() {
		let cores = std::thread::available_parallelism()
			.map(|n| n.get())
			.unwrap_or(2) as i32;
		let half = (cores / 2).max(1);
		for concurrent in [1, 2, 4, 8, 16] {
			let b = ProxyTask::transcode_thread_budget(concurrent);
			assert!((1..=8).contains(&b));
			assert!(
				b * concurrent <= half.max(1) || b == 1,
				"concurrency {concurrent} x threads {b} exceeds half of {cores} cores"
			);
		}
		// 并发为 1 时独占一半核。
		assert_eq!(ProxyTask::transcode_thread_budget(1), half.clamp(1, 8));
	}

	/// End-to-end: transcode a real 4K H.265 4:2:2 10-bit file (the
	/// class of media the 4K playback lag was reported on) to a 720p
	/// proxy. Skipped when the fixture or ffmpeg is missing.
	#[test]
	fn proxy_task_transcodes_hevc_422_10bit() {
		let src = "/tmp/oakperf/hevc422-4k.mp4";
		if !std::path::Path::new(src).exists() {
			eprintln!("fixture missing; skipping");
			return;
		}
		let ffmpeg = ProxyManager::find_ffmpeg("");
		if ffmpeg.is_empty() {
			eprintln!("ffmpeg not found; skipping");
			return;
		}
		let out = "/tmp/oakperf/proxy-720p.mp4";
		let _ = std::fs::remove_file(out);
		let request = oak_codec::task::TaskRequest {
			kind: oak_codec::task::TaskKind::Proxy,
			input_filename: src,
			output_filename: out,
			stream_index: 0,
			sample_rate: 0,
			channel_layout: 0,
			sample_format: 0,
			proxy_width: 1280,
			proxy_height: 720,
		};
		let params = ProxyParams {
			width: 1280,
			height: 720,
			divider: 1,
			version: 1,
			crf: 23,
			include_audio: true,
			extension: "mp4".to_string(),
			preset: "veryfast".to_string(),
		};
		let mut proxy_task = ProxyTask::new(&request, params);
		let mut task = Task::new("probe", None);
		proxy_task
			.run(&mut task)
			.expect("the proxy transcode succeeds on 4K HEVC 4:2:2 10-bit");
		let meta = std::fs::metadata(out).expect("the proxy file exists");
		assert!(meta.len() > 0, "the proxy file is non-empty");
	}
}
