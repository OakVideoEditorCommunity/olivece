# oakaudio 类覆盖映射表（C++ audio 模块 → oakaudio Rust crate）

> 逐类盘点 `src/audio/src` 与 `src/audio/c_api`。每一行标注 Rust 侧落点。
> `// CPP-PARITY` 注释义务不变：凡承载布局/数值/边角行为的地方，标出
> C++ 文件:行号。本表是初稿；实现阶段据实核对。

## 1. AudioManager（manager.rs）

| C++ | Rust 落点 |
|---|---|
| `AudioManager::create_instance` / `destroy_instance` / `instance` | `manager::create_instance` / `destroy_instance` / `instance`（`OnceLock<Mutex<ManagerInner>>` 单例） |
| `set_output_notify_interval` / `set_output_notify_callback` | `manager::set_output_notify_interval`（notify 回调经 guard 从 PortAudio 线程调用，见 previewdevice.rs） |
| `push_to_output` / `clear_buffered_output` / `stop_output` | `manager::push_to_output` / `clear_buffered_output` / `stop_output` |
| `seconds` / `reset_output_clock` | `manager::seconds` / `reset_output_clock`（播放时钟补偿输出延迟） |
| `get/set_output_device` / `get/set_input_device` | `manager` 设备访问器（PaDeviceIndex，-1 = paNoDevice） |
| `hard_reset` | `manager::hard_reset`（关流并重初始化 PortAudio） |
| `start_recording` / `stop_recording` | `manager` 录音（经 `bridge::codec` OakEncoder，恒定 interleaved f32） |
| `find_config_device_by_name` / `find_device_by_name`（static） | `manager::find_config_device_by_name_s` / `find_device_by_name_s` |
| `get_port_audio_params` / `get_port_audio_sample_format`（私有） | `manager` 内部（映射 AudioParams ↔ PortAudio；`// CPP-PARITY` 标注格式映射） |

## 2. AudioProcessor（processor.rs）

| C++ | Rust 落点 |
|---|---|
| `open(from,to,tempo)` / `close` / `is_open` | `processor::open` / `close` / `is_open`（FBAudioGraphConfig 组装） |
| `convert` | `processor::convert`（planar f32 进/出；`fb_audio_graph_push` + `fb_audio_graph_pull`） |
| `flush` | `processor::flush`（`fb_audio_graph_push` channel_data==NULL） |
| `from()` / `to()` | `processor` 保存的 `AudioParams` |

## 3. AudioSynchronizer（synchronizer.rs，纯静态）

| C++ | Rust 落点 |
|---|---|
| `place_by_source_time` | `synchronizer::place_by_source_time` |
| `place_by_waveform_offset` | `synchronizer::place_by_waveform_offset` |

## 4. AudioLevelMeter（levelmeter.rs，纯静态）

| C++ | Rust 落点 |
|---|---|
| `analyze_sample_buffer` | `levelmeter::analyze`（peak/RMS/VU 阈值与 `-200` 地板；`// CPP-PARITY` 标注） |
| `linear_to_db` / `power_to_lufs`（私有） | `levelmeter` 内部（BS.1770 LUFS，无 K-weighting） |

## 5. AudioVisualWaveform（waveform.rs）

| C++ | Rust 落点 |
|---|---|
| 构造 / `channel_count` / `set_channel_count` / `length` | `waveform::Waveform` + 访问器 |
| `overwrite_samples` | `waveform::overwrite_samples`（mipmap 展开，`k_minimum/maximum_sample_rate`） |
| `overwrite_sums` / `overwrite_silence` | `waveform::overwrite_sums` / `overwrite_silence` |
| `trim_in` / `mid` / `resize` / `trim_range` | `waveform::trim_in` / `mid` / `resize` / `trim_range` |
| `get_summary_from_time` | `waveform::get_summary` |
| `sum_samples` / `re_sum_samples`（static） | `waveform::sum_samples` / `re_sum_samples` |
| mipmap 内部（`overwrite_samples_from_*` / `get_mipmap_for_scale` / `time_to_samples` / `validate_virtual_start`） | `waveform` 内部 |
| 全文件提取（c_api `oakaudio_waveform_extract`） | `waveform::extract`（经 `bridge::codec` decoder + `bridge::ffmpeg`） |
| `SamplePerChannel` POD | 与 `oakaudio_min_max` `static_assert` 对齐（`// CPP-PARITY: c_api/waveform.cpp`） |

## 6. AudioWaveformSync（waveformsync.rs，纯静态）

| C++ | Rust 落点 |
|---|---|
| `extract_rms_envelope` | `waveformsync::extract_rms_envelope` |
| `estimate_offset`（SampleBuffer 版） | `waveformsync::estimate_offset`（内部先提取 envelope） |
| `estimate_envelope_offset`（两个重载） | `waveformsync::estimate_envelope_offset`（valid 掩码版为权威） |
| `estimate_stretch_and_offset` | `waveformsync::estimate_stretch_and_offset`（O(rates*lags*overlap)） |

## 7. PreviewAudioDevice（previewdevice.rs，header-only）

| C++ | Rust 落点 |
|---|---|
| `read` / `write` | `previewdevice::PreviewAudioDevice::read` / `write`（回调侧 pull） |
| `set_params` / `bytes_per_frame` / `set_bytes_per_frame` | `previewdevice`（AudioParams → bytes_per_frame） |
| `set_notify_interval` / `set_notify_callback` | `previewdevice`（notify 回调，锁外触发） |
| `clear` | `previewdevice::clear` |
| `add_output_frames` / `output_frames_consumed` / `reset_output_frames` | `previewdevice`（atomic 播放时钟） |

## 8. audio_config 命名空间（config.rs，不是类）

| C++ | Rust 落点 |
|---|---|
| `output_buffer_size()` | `config::output_buffer_size`（`oak_core_config_get_int(nullptr,"AudioOutputBufferSize",0)`） |
| `device_name(is_output_device)` | `config::device_name`（`oak_core_config_get` 两阶段；key = "AudioOutput"/"AudioInput"） |

## 9. 刻意不迁移（drop）

| C++ | 理由 |
|---|---|
| `get_port_audio_params` 的 PortAudio 平台细节 | 归 `bridge::ffmpeg`/PortAudio 侧；Rust 保留语义与默认布局兜底 |
| Qt 常量（`qFuzzyIsNull` 等）内联展开 | 语义内联为 Rust 比较；`// CPP-PARITY` 标注 |
| `draw_sample`/`draw_waveform`（QPainter） | UI 绘制归 facade/app；crate 只存/汇总数据 |
