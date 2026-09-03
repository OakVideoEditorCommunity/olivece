# M5 · oakcodec 拆分手册

> 内容：`engine/codec/`（decoder/encoder/frame/waveform/proxymanager/
> conformmanager）。
> 依赖：common 12、render 11（M3.5 下沉后剩 ~3）、task 5、node 3、
> core 4。
> 拆分顺序第 5 位。

## 1. 目标形态

```
oakcodec/
  include/oakcodec/{frame.h, decoder.h, encoder.h, waveform.h, proxy.h,
                   types.h, export.h}
  src/
  tests/  # oakcodec_gtest
```

## 2. 冻结 C API

### 2.1 `oakcodec/frame.h`（render 7 次引 codec/frame.h）

```c
typedef struct OakCodecFrame OakCodecFrame;
OAKCD_API OakCodecFrame *oakcodec_frame_init(void);
OAKCD_API OakCodecFrame *oakcodec_frame_init_copy(const OakCodecFrame *o);
OAKCD_API void oakcodec_frame_free(OakCodecFrame *f);
OAKCD_API OakCodecFrame *oakcodec_frame_retain(OakCodecFrame *f); /* +1 */
OAKCD_API int oakcodec_frame_set_params(OakCodecFrame *f,
	const oak_video_params *p);
OAKCD_API int oakcodec_frame_get_params(const OakCodecFrame *f,
	oak_video_params *out);
OAKCD_API int oakcodec_frame_allocate(OakCodecFrame *f);
OAKCD_API void *oakcodec_frame_data(OakCodecFrame *f);
OAKCD_API int oakcodec_frame_linesize(const OakCodecFrame *f);
OAKCD_API int64_t oakcodec_frame_timestamp(const OakCodecFrame *f);
OAKCD_API void oakcodec_frame_set_timestamp(OakCodecFrame *f, int64_t ts);
```

### 2.2 `oakcodec/decoder.h`

```c
typedef struct OakCodecDecoder OakCodecDecoder;
OAKCD_API OakCodecDecoder *oakcodec_decoder_init(const char *filename);
OAKCD_API void oakcodec_decoder_free(OakCodecDecoder *d);
OAKCD_API int oakcodec_decoder_stream_count(const OakCodecDecoder *d,
	int media_type /* 0=video 1=audio */);
OAKCD_API int oakcodec_decoder_get_video_stream(const OakCodecDecoder *d,
	int index, oak_video_params *out, int64_t *duration_ts);
OAKCD_API int oakcodec_decoder_get_audio_stream(const OakCodecDecoder *d,
	int index, int *sample_rate, uint64_t *layout, int *format);
OAKCD_API OakCodecFrame *oakcodec_decoder_decode_video(
	OakCodecDecoder *d, int stream, int64_t ts);   /* NULL=EOF/错误 */
OAKCD_API int oakcodec_decoder_decode_audio(OakCodecDecoder *d,
	int stream, int64_t ts, float *buf, int frame_count);
OAKCD_API int oakcodec_decoder_last_error(OakCodecDecoder *d,
	char *buf, int n);
```

### 2.3 `oakcodec/encoder.h`

```c
typedef struct OakCodecEncoder OakCodecEncoder;
OAKCD_API OakCodecEncoder *oakcodec_encoder_init(
	const oak_encoding_params *params);   /* POD 照 oakengine/encoding.h */
OAKCD_API void oakcodec_encoder_free(OakCodecEncoder *e);
OAKCD_API int oakcodec_encoder_write_video(OakCodecEncoder *e,
	const OakCodecFrame *f);
OAKCD_API int oakcodec_encoder_write_audio(OakCodecEncoder *e,
	const float *samples, int frame_count);
OAKCD_API int oakcodec_encoder_flush(OakCodecEncoder *e);
OAKCD_API int oakcodec_encoder_last_error(OakCodecEncoder *e,
	char *buf, int n);
```

### 2.4 `oakcodec/waveform.h`、`oakcodec/proxy.h`

waveform：`oakcodec_waveform_extract(filename, stream, out_min_max_pairs,
progress_cb)`（AudioWaveformCache 的磁盘格式不变）。
proxy：照 `oakengine/proxy.h` 模板（get_or_start/state/cancel）。

## 3. 切割点

| 现状 | 处理 |
|---|---|
| codec → render/ 11 | videoparams/subtitleparams/colortransform 已随 M3.5 下沉 oak_core；剩 renderer.h(2)、framemanager.h(1) → framemanager 是 codec 内部缓存编排，**随 codec 一起走**（从 render/ 移入 oakcodec/src，纯文件移动，它本来就主要服务 codec） |
| codec → task/ 5（taskmanager/conform/proxy 编排） | proxy/conform 对 TaskManager 的引用改为 01 §4 回调注册（`oakcodec_set_task_submit_cb`），Task 对象创建上移 oaktask（M8），oakcodec 只调回调 |
| codec → node/ 3 | 经 oaknode C ABI（M3 已就位） |

## 4. 测试（映射 03 §2/§3）

- frame：params 往返、allocate/data/linesize、retain/free 引用计数。
- decoder：`tests/demo.mp4` 开流、stream 枚举、decode 首帧非空、
  错误路径（不存在文件 → E_NOT_FOUND + last_error 非空）。
- encoder：写小 mp4（照 oak_cli_transcode 的参数），flush 后文件
  可再被 decoder 打开（往返）。
- waveform：demo.mp4 提取返回非空且长度与时长一致（容差断言）。
- proxy：mock 免（03 §4）——用 demo.mp4 低分辨率参数真跑一次或
  按环境跳过。
- `oakcodec_debug_alive_count()` 泄漏断言。
