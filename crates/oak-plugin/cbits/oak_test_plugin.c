/*
 * Oak Video Editor - Non-Linear Video Editor
 * Copyright (C) 2026 Oak Team
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

/*
 * 最小测试插件（M11 §2.4 的交付物；build.rs 编译为共享库，
 * 运行时装配成 oak-test-plugin.ofx.bundle 供 host 扫描）。
 *
 * filter 上下文：
 *  - 参数：gain（Double，默认 0）
 *  - clip：Source（输入，RGBA）、Output
 *  - describe/describeInContext 建参数与 clip；
 *  - getClipPreferences：全链路 F32+RGBA、帧率 24；
 *  - render：把输出图像填成常量 0.5（RGBA float），alpha=1；
 *  - getRoD：project size；getRoI：原样返回 region；
 *  - isIdentity：不设 → 非透传；
 *  - begin/endSequenceRender：记日志（状态码 OK）。
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

/* Windows 上插件 DLL 带自己的 CRT 环境块：宿主 Rust 侧的
 * std::env::set_var 走 SetEnvironmentVariableW，只更新 Win32 进程
 * 环境，插件里 getenv 读的 CRT `_environ` 看不到它。marker 路径
 * 统一经 Win32 进程环境读取（POSIX 下就是 getenv）。 */
static const char *marker_env(const char *name)
{
#ifdef _WIN32
    static char buf[32768];
    DWORD n = GetEnvironmentVariableA(name, buf, (DWORD)sizeof buf);
    if (n == 0 || n >= sizeof buf)
        return NULL;
    return buf;
#else
    return getenv(name);
#endif
}

#include "ofxCore.h"
#include "ofxColour.h"
#include "ofxDrawSuite.h"
#include "ofxGPURender.h"
#include "ofxImageEffect.h"
#include "ofxInteract.h"
#include "ofxKeySyms.h"
#include "ofxMessage.h"
#include "ofxParam.h"
#include "ofxProgress.h"
#include "ofxProperty.h"

/* GL 端到端测试的真实绘制需要 GL 命令（仅 macOS 有真实 CGL 桥；
 * Linux/Windows 的 GL 桥是 stub，绘制代码随 __APPLE__ 排除）。 */
#ifdef __APPLE__
#include <OpenGL/gl.h>
#endif

/* kOfxImageEffectGLFormatRGBA 在 vendored ofxOpenGLRender.h 是 stub
 * 未收录（OpenFX 1.4 规范名），按规范定义。kOfxImageEffectPropIsIdentity
 * 同理（vendored ofxImageEffect.h 只文档化了该属性，未给宏）。 */
#define GL_FORMAT_RGBA "OfxImageEffectGLFormatRGBA"
#define kOfxImageEffectPropIsIdentity "OfxImageEffectPropIsIdentity"

static OfxHost *g_host = NULL;
static const OfxPropertySuiteV1 *g_propSuite = NULL;
static const OfxImageEffectSuiteV1 *g_imageEffectSuite = NULL;
static const OfxParameterSuiteV1 *g_paramSuite = NULL;
static const OfxMessageSuiteV1 *g_messageSuite = NULL;
static const OfxProgressSuiteV1 *g_progressSuite = NULL;
static const OfxImageEffectOpenGLRenderSuiteV1 *g_glSuite = NULL;
static const OfxDrawSuiteV1 *g_drawSuite = NULL;
static const OfxInteractSuiteV1 *g_interactSuite = NULL;

/* 宿主扩展（任务契约；vendored ofxInteract.h 未收录的 action 名与属性
 * 名）。kOfxInteractPropViewportSize 取 OFX 1.3 的规范名
 * "OfxInteractPropViewport"（vendored 1.5 头已删）；BackgroundImage 与
 * NewInteract/Idle 是本宿主扩展。 */
#ifndef kOfxActionNewInteract
#define kOfxActionNewInteract "OfxActionNewInteract"
#endif
#ifndef kOfxInteractActionIdle
#define kOfxInteractActionIdle "OfxInteractActionIdle"
#endif
#ifndef kOfxInteractPropViewportSize
#define kOfxInteractPropViewportSize "OfxInteractPropViewport"
#endif
#ifndef kOfxInteractPropBackgroundImage
#define kOfxInteractPropBackgroundImage "OfxInteractPropBackgroundImage"
#endif

/* push button 的 instanceChanged 调用计数（宿主按下按钮后应路由
 * kOfxActionInstanceChanged/UserEdited 到这里）。 */
static int g_button_instance_changed = 0;

/* ---------- suite 便捷封装 ---------- */

/* 记录一次 push button 的 instanceChanged：计数自增，并在
 * OAK_TEST_PLUGIN_INSTANCECHANGED_MARKER 指向的文件追加一行（宿主
 * 测试断言用；未设环境变量时静默）。 */
static void record_button_instance_changed(void)
{
    g_button_instance_changed++;
    const char *marker = marker_env("OAK_TEST_PLUGIN_INSTANCECHANGED_MARKER");
    if (!marker)
        return;
    FILE *f = fopen(marker, "a");
    if (!f)
        return;
    fprintf(f, "button instanceChanged count=%d\n", g_button_instance_changed);
    fclose(f);
}

static OfxStatus propSetString(OfxPropertySetHandle h, const char *name, int index, const char *v)
{
    return g_propSuite->propSetString(h, name, index, v);
}

static OfxStatus propSetStringN(OfxPropertySetHandle h, const char *name, int count, const char **v)
{
    return g_propSuite->propSetStringN(h, name, count, v);
}

static OfxStatus propSetPointer(OfxPropertySetHandle h, const char *name, int index, void *v)
{
    return g_propSuite->propSetPointer(h, name, index, v);
}

static OfxStatus propSetDoubleN(OfxPropertySetHandle h, const char *name, int count, const double *v)
{
    return g_propSuite->propSetDoubleN(h, name, count, v);
}

static OfxStatus propGetDouble(OfxPropertySetHandle h, const char *name, int index, double *v)
{
    return g_propSuite->propGetDouble(h, name, index, v);
}

static OfxStatus propGetInt(OfxPropertySetHandle h, const char *name, int index, int *v)
{
    return g_propSuite->propGetInt(h, name, index, v);
}

static OfxStatus propGetIntN(OfxPropertySetHandle h, const char *name, int count, int *v)
{
    return g_propSuite->propGetIntN(h, name, count, v);
}

static OfxStatus propGetPointer(OfxPropertySetHandle h, const char *name, int index, void **v)
{
    return g_propSuite->propGetPointer(h, name, index, v);
}

/* 协商期 per-clip 属性名：前缀_clip */
static void setClipPref(OfxPropertySetHandle out, const char *prefix, const char *clip, const char *value)
{
    char name[256];
    snprintf(name, sizeof(name), "%s_%s", prefix, clip);
    propSetString(out, name, 0, value);
}

/* ---------- setHost（mandatory 第一个调用） ---------- */

static void setHost(OfxHost *host)
{
    g_host = host;
    if (!host)
        return;
    g_propSuite = (const OfxPropertySuiteV1 *)host->fetchSuite(host->host, kOfxPropertySuite, 1);
    g_imageEffectSuite =
        (const OfxImageEffectSuiteV1 *)host->fetchSuite(host->host, kOfxImageEffectSuite, 1);
    g_paramSuite = (const OfxParameterSuiteV1 *)host->fetchSuite(host->host, kOfxParameterSuite, 1);
    g_messageSuite = (const OfxMessageSuiteV1 *)host->fetchSuite(host->host, kOfxMessageSuite, 1);
    g_progressSuite = (const OfxProgressSuiteV1 *)host->fetchSuite(host->host, kOfxProgressSuite, 1);
    g_glSuite = (const OfxImageEffectOpenGLRenderSuiteV1 *)host->fetchSuite(
        host->host, kOfxOpenGLRenderSuite, 1);
    g_drawSuite = (const OfxDrawSuiteV1 *)host->fetchSuite(host->host, kOfxDrawSuite, 1);
    g_interactSuite = (const OfxInteractSuiteV1 *)host->fetchSuite(host->host, kOfxInteractSuite, 1);
}

/* ---------- action 实现 ---------- */

static OfxStatus actionDescribe(const void *handle, int is_gl)
{
    const char *comps[] = { kOfxImageComponentRGBA, kOfxImageComponentRGB, kOfxImageComponentAlpha };

    propSetString(handle, kOfxPropLabel, 0, "Oak Test Plugin");
    propSetStringN(handle, kOfxImageEffectPropSupportedContexts, 1,
                   &(const char *){ kOfxImageEffectContextFilter });

    /* ofxColour（M11 §4）：声明 OCIO 色彩管理能力与可用配置。 */
    propSetString(handle, kOfxImageEffectPropColourManagementStyle, 0,
                  kOfxImageEffectColourManagementOCIO);
    {
        const char *configs[] = { "ofx-native-v1.5_aces-v1.3_ocio-v2.3" };
        propSetStringN(handle, kOfxImageEffectPropColourManagementAvailableConfigs, 1, configs);
    }

    /* GL 能力（M11 §4）：GL 变体声明 "true" + 支持位深（F32）。
     * CPU 变体保持默认 "false"。 */
    if (is_gl) {
        propSetString(handle, kOfxImageEffectPropOpenGLRenderSupported, 0, "true");
        {
            const char *depths[] = { kOfxBitDepthFloat };
            propSetStringN(handle, kOfxOpenGLPropPixelDepth, 1, depths);
        }
    }

    /* 参数族：gain（Double，带 display min/max）、mode（Choice，
     * 两选项）、debug（Double，secret）、label（String）。 */
    OfxPropertySetHandle paramProps = NULL;
    OfxStatus st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypeDouble, "gain", &paramProps);
    if (st != kOfxStatOK)
        return st;
    propSetString(paramProps, kOfxPropLabel, 0, "Gain");
    {
        const double min = -2.0, max = 2.0;
        g_propSuite->propSetDouble(paramProps, kOfxParamPropDisplayMin, 0, min);
        g_propSuite->propSetDouble(paramProps, kOfxParamPropDisplayMax, 0, max);
    }

    st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypeChoice, "mode", &paramProps);
    if (st != kOfxStatOK)
        return st;
    propSetString(paramProps, kOfxPropLabel, 0, "Mode");
    {
        const char *options[] = { "Fast", "High" };
        g_propSuite->propSetStringN(paramProps, kOfxParamPropChoiceOption, 2, options);
    }

    st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypeDouble, "debug", &paramProps);
    if (st != kOfxStatOK)
        return st;
    g_propSuite->propSetInt(paramProps, kOfxParamPropSecret, 0, 1);

    st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypeString, "label", &paramProps);
    if (st != kOfxStatOK)
        return st;
    propSetString(paramProps, kOfxPropLabel, 0, "Label");

    /* push button：无值，按下经 instanceChanged (UserEdited) 通知
     * 插件（ofxCore.h kOfxActionInstanceChanged）。 */
    st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypePushButton, "button", &paramProps);
    if (st != kOfxStatOK)
        return st;
    propSetString(paramProps, kOfxPropLabel, 0, "Button");

    /* clip：Source + Output。 */
    OfxPropertySetHandle clipProps = NULL;
    st = g_imageEffectSuite->clipDefine((OfxImageEffectHandle)handle, "Source", &clipProps);
    if (st != kOfxStatOK)
        return st;
    propSetStringN(clipProps, kOfxImageEffectPropSupportedComponents, 3, comps);
    propSetString(clipProps, kOfxPropLabel, 0, "Source");

    st = g_imageEffectSuite->clipDefine((OfxImageEffectHandle)handle, "Output", &clipProps);
    if (st != kOfxStatOK)
        return st;
    propSetStringN(clipProps, kOfxImageEffectPropSupportedComponents, 3, comps);
    return kOfxStatOK;
}

static OfxStatus actionGetClipPreferences(OfxPropertySetHandle outArgs)
{
    setClipPref(outArgs, "OfxImageClipPropComponents", "Source", kOfxImageComponentRGBA);
    setClipPref(outArgs, "OfxImageClipPropDepth", "Source", kOfxBitDepthFloat);
    setClipPref(outArgs, "OfxImageClipPropComponents", "Output", kOfxImageComponentRGBA);
    setClipPref(outArgs, "OfxImageClipPropDepth", "Output", kOfxBitDepthFloat);
    g_propSuite->propSetDouble(outArgs, kOfxImageEffectPropFrameRate, 0, 24.0);
    g_propSuite->propSetString(outArgs, kOfxImageClipPropFieldOrder, 0, kOfxImageFieldNone);
    return kOfxStatOK;
}

static OfxStatus actionGetRoD(OfxPropertySetHandle outArgs)
{
    const double rod[4] = { 0.0, 0.0, 1920.0, 1080.0 };
    propSetDoubleN(outArgs, kOfxImageEffectPropRegionOfDefinition, 4, rod);
    return kOfxStatOK;
}

static OfxStatus actionGetRoI(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs,
                              OfxPropertySetHandle outArgs)
{
    /* 对每个输入 clip 回填 region（此处原样返回 Output 的 window 即可）。 */
    OfxImageClipHandle clip = NULL;
    OfxPropertySetHandle clipProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Source", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;
    double region[4] = { 0.0, 0.0, 0.0, 0.0 };
    st = g_propSuite->propGetDoubleN(inArgs, kOfxImageEffectPropRegionOfInterest, 4, region);
    if (st != kOfxStatOK)
        return st;
    char name[256];
    snprintf(name, sizeof(name), "OfxImageEffectPropRegionOfInterest_%s", "Source");
    st = propSetDoubleN(outArgs, name, 4, region);
    fprintf(stderr, "DBG getRoI read=(%g,%g,%g,%g) set st=%d\n", region[0], region[1], region[2], region[3], st);
    if (st != kOfxStatOK)
        return st;
    return kOfxStatOK;
}

static OfxStatus actionRender(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs)
{
    double time = 0.0;
    propGetDouble(inArgs, kOfxPropTime, 0, &time);

    OfxImageClipHandle clip = NULL;
    OfxPropertySetHandle clipProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Output", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;

    OfxPropertySetHandle image = NULL;
    st = g_imageEffectSuite->clipGetImage(clip, time, NULL, &image);
    if (st != kOfxStatOK)
        return st;

    void *data = NULL;
    int rowBytes = 0;
    int bounds[4] = { 0, 0, 0, 0 };
    propGetPointer(image, kOfxImagePropData, 0, &data);
    propGetInt(image, kOfxImagePropRowBytes, 0, &rowBytes);
    propGetIntN(image, kOfxImagePropBounds, 4, bounds);

    /* 进度：Start → Update(0.5)（取消则中止）→ End。 */
    if (g_progressSuite) {
        g_progressSuite->progressStart((OfxImageEffectHandle)inst, "render");
        OfxStatus ps = g_progressSuite->progressUpdate((OfxImageEffectHandle)inst, 0.5);
        if (ps != kOfxStatOK) {
            g_progressSuite->progressEnd((OfxImageEffectHandle)inst);
            return kOfxStatFailed;
        }
    }

    int w = bounds[2] - bounds[0];
    int h = bounds[3] - bounds[1];
    if (data && w > 0 && h > 0) {
        /* 常量 0.5（RGBA float）；alpha=1。 */
        for (int y = 0; y < h; y++) {
            float *row = (float *)((char *)data + (size_t)y * (size_t)rowBytes);
            for (int x = 0; x < w; x++) {
                row[x * 4 + 0] = 0.5f;
                row[x * 4 + 1] = 0.5f;
                row[x * 4 + 2] = 0.5f;
                row[x * 4 + 3] = 1.0f;
            }
        }
    }

    g_imageEffectSuite->clipReleaseImage(image);
    if (g_progressSuite) {
        g_progressSuite->progressEnd((OfxImageEffectHandle)inst);
    }
    return kOfxStatOK;
}

/* ---------- 慢速变体（M3）：取消 e2e 测试 ---------- */

/* progressUpdate 之间的睡眠（宿主取消在渲染中置位，下一次
 * progressUpdate 返回非 OK，插件立即中止）。 */
static void slow_sleep_ms(int ms)
{
#ifdef _WIN32
    Sleep((DWORD)ms);
#else
    usleep((useconds_t)ms * 1000);
#endif
}

/* 100 次 progressUpdate x 20ms（约 2 秒）：把输出填成常量 0.25。
 * 任一次 progressUpdate 返回非 OK（宿主取消）→ kOfxStatFailed。 */
static OfxStatus actionRenderSlow(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs)
{
    double time = 0.0;
    propGetDouble(inArgs, kOfxPropTime, 0, &time);

    OfxImageClipHandle clip = NULL;
    OfxPropertySetHandle clipProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Output", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;

    OfxPropertySetHandle image = NULL;
    st = g_imageEffectSuite->clipGetImage(clip, time, NULL, &image);
    if (st != kOfxStatOK)
        return st;

    void *data = NULL;
    int rowBytes = 0;
    int bounds[4] = { 0, 0, 0, 0 };
    propGetPointer(image, kOfxImagePropData, 0, &data);
    propGetInt(image, kOfxImagePropRowBytes, 0, &rowBytes);
    propGetIntN(image, kOfxImagePropBounds, 4, bounds);

    if (g_progressSuite) {
        g_progressSuite->progressStart((OfxImageEffectHandle)inst, "slow-render");
        for (int i = 0; i < 100; i++) {
            OfxStatus ps = g_progressSuite->progressUpdate((OfxImageEffectHandle)inst,
                                                           (double)(i + 1) / 100.0);
            if (ps != kOfxStatOK) {
                g_progressSuite->progressEnd((OfxImageEffectHandle)inst);
                return kOfxStatFailed;
            }
            slow_sleep_ms(20);
        }
    }

    int w = bounds[2] - bounds[0];
    int h = bounds[3] - bounds[1];
    if (data && w > 0 && h > 0) {
        for (int y = 0; y < h; y++) {
            float *row = (float *)((char *)data + (size_t)y * (size_t)rowBytes);
            for (int x = 0; x < w; x++) {
                row[x * 4 + 0] = 0.25f;
                row[x * 4 + 1] = 0.25f;
                row[x * 4 + 2] = 0.25f;
                row[x * 4 + 3] = 1.0f;
            }
        }
    }

    g_imageEffectSuite->clipReleaseImage(image);
    if (g_progressSuite) {
        g_progressSuite->progressEnd((OfxImageEffectHandle)inst);
    }
    return kOfxStatOK;
}

/* 慢速变体的效果入口：render 走慢速路径，其余 action 委托基础实现。 */
static OfxStatus mainEntry(const char *action, const void *handle,
                           OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs);

static OfxStatus mainEntrySlow(const char *action, const void *handle,
                               OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxImageEffectActionRender) == 0) {
        return actionRenderSlow((OfxImageEffectHandle)handle, inArgs);
    }
    return mainEntry(action, handle, inArgs, outArgs);
}
/* ---------- ofxColour（M11 §4）：GetOutputColourspace ---------- */

static OfxStatus actionGetOutputColourspace(OfxPropertySetHandle inArgs,
                                            OfxPropertySetHandle outArgs)
{
    /* 优先采纳宿主偏好的第一个色彩空间；否则交叉引用 Source clip
     * （ofxColour.h：cross-reference 是合法回写，宿主须解析）。 */
    int n = 0;
    g_propSuite->propGetDimension(inArgs, kOfxImageClipPropPreferredColourspaces, &n);
    if (n > 0) {
        char *pref = NULL;
        OfxStatus st = g_propSuite->propGetString(
            inArgs, kOfxImageClipPropPreferredColourspaces, 0, &pref);
        if (st == kOfxStatOK && pref) {
            propSetString(outArgs, kOfxImageClipPropColourspace, 0, pref);
            return kOfxStatOK;
        }
    }
    propSetString(outArgs, kOfxImageClipPropColourspace, 0, "OfxColourspace_Source");
    return kOfxStatOK;
}

/* ---------- isIdentity 变体：透传 Source ---------- */

static OfxStatus actionIsIdentity(OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    /* 恒透传 Source（时间不变）——宿主应短路 render 直接拷贝 Source
     * 的帧。 */
    propSetString(outArgs, kOfxImageEffectPropIsIdentity, 0, "Source");
    return kOfxStatOK;
}

/* ---------- GL 变体（M11 §4）：attach/detach 与 GL render ---------- */

static OfxStatus actionGLAttached(OfxImageEffectHandle handle)
{
    if (g_messageSuite) {
        g_messageSuite->message(handle, kOfxMessageMessage, "gl-test", "gl-attached");
    }
    return kOfxStatOK;
}

static OfxStatus actionGLDetached(OfxImageEffectHandle handle)
{
    if (g_messageSuite) {
        g_messageSuite->message(handle, kOfxMessageMessage, "gl-test", "gl-detached");
    }
    return kOfxStatOK;
}

/* GL render：经 OpenGL suite 取 Source 与 Output 纹理并上报索引
 * （宿主侧测试经 message 捕获断言）。GL 未使能时回退 CPU render。 */
static void draw_gl_test_pattern(void)
{
#ifdef __APPLE__
    /* 把当前绑定帧缓冲（宿主为输出帧挂的 FBO）清成已知颜色。 */
    glClearColor(0.1f, 0.2f, 0.3f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
#else
    (void)0;
#endif
}

static OfxStatus actionRenderGL(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs)
{
    int gl_enabled = 0;
    g_propSuite->propGetInt(inArgs, kOfxImageEffectPropOpenGLEnabled, 0, &gl_enabled);
    if (!gl_enabled) {
        return actionRender(inst, inArgs);
    }
    if (!g_glSuite) {
        return kOfxStatErrMissingHostFeature;
    }

    double time = 0.0;
    propGetDouble(inArgs, kOfxPropTime, 0, &time);

    /* 输入 clip：clipLoadTexture（请求 RGBA）。 */
    OfxImageClipHandle clip = NULL;
    OfxPropertySetHandle clipProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Source", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;
    OfxPropertySetHandle tex = NULL;
    st = g_glSuite->clipLoadTexture(clip, time, GL_FORMAT_RGBA, NULL, &tex);
    if (st != kOfxStatOK)
        return st;
    int src_index = 0;
    g_propSuite->propGetInt(tex, kOfxImageEffectPropOpenGLTextureIndex, 0, &src_index);
    if (g_messageSuite) {
        g_messageSuite->message(inst, kOfxMessageMessage, "gl-test", "gl-source-index=%d", src_index);
    }
    st = g_glSuite->clipFreeTexture(tex);
    if (st != kOfxStatOK)
        return st;

    /* 输出 clip：clipLoadTexture(Output)（format 忽略）。 */
    st = g_imageEffectSuite->clipGetHandle(inst, "Output", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;
    tex = NULL;
    st = g_glSuite->clipLoadTexture(clip, time, NULL, NULL, &tex);
    if (st != kOfxStatOK)
        return st;
    int out_index = 0;
    g_propSuite->propGetInt(tex, kOfxImageEffectPropOpenGLTextureIndex, 0, &out_index);
    if (g_messageSuite) {
        g_messageSuite->message(inst, kOfxMessageMessage, "gl-test", "gl-output-index=%d", out_index);
    }
    st = g_glSuite->clipFreeTexture(tex);
    if (st != kOfxStatOK)
        return st;

    /* 真实绘制：把宿主已绑定的输出 FBO 清成已知颜色
     * (0.1, 0.2, 0.3, 1.0)——宿主 glReadPixels 回读后应得到该颜色，
     * 这是"插件真实走 GL 路径且输出正确"的端到端证据。 */
    draw_gl_test_pattern();

    return kOfxStatOK;
}

static OfxStatus mainEntry(const char *action, const void *handle, OfxPropertySetHandle inArgs,
                           OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        return actionDescribe(handle, 0);
    }
    if (strcmp(action, kOfxActionCreateInstance) == 0) {
        /* 经 message suite 发一条（facade 处理器断言用）。 */
        if (g_messageSuite) {
            g_messageSuite->message(handle, kOfxMessageMessage, "test-plugin",
                                    "created id=%d", 7);
        }
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionDestroyInstance) == 0 ||
        strcmp(action, kOfxActionLoad) == 0 ||
        strcmp(action, kOfxActionUnload) == 0 ||
        strcmp(action, kOfxImageEffectActionBeginSequenceRender) == 0 ||
        strcmp(action, kOfxImageEffectActionEndSequenceRender) == 0 ||
        strcmp(action, kOfxImageEffectActionIsIdentity) == 0) {
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxImageEffectActionGetClipPreferences) == 0) {
        return actionGetClipPreferences(outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetRegionOfDefinition) == 0) {
        return actionGetRoD(outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetRegionsOfInterest) == 0) {
        return actionGetRoI((OfxImageEffectHandle)handle, inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetOutputColourspace) == 0) {
        return actionGetOutputColourspace(inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionRender) == 0) {
        return actionRender((OfxImageEffectHandle)handle, inArgs);
    }
    if (strcmp(action, kOfxActionInstanceChanged) == 0) {
        /* 宿主按下 push button：kOfxPropName == "button"、
         * kOfxPropChangeReason == kOfxChangeUserEdited。只对 button
         * 计数（其余参数变更静默）。 */
        char *name = NULL;
        g_propSuite->propGetString(inArgs, kOfxPropName, 0, &name);
        if (name && strcmp(name, "button") == 0) {
            record_button_instance_changed();
        }
        return kOfxStatOK;
    }
    return kOfxStatReplyDefault;
}

/* GL 变体入口：describe 带 GL 声明；render 走 GL suite；attach/
 * detach 上报。 */
static OfxStatus mainEntryGL(const char *action, const void *handle,
                             OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        return actionDescribe(handle, 1);
    }
    if (strcmp(action, kOfxActionCreateInstance) == 0) {
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionDestroyInstance) == 0 ||
        strcmp(action, kOfxActionLoad) == 0 ||
        strcmp(action, kOfxActionUnload) == 0 ||
        strcmp(action, kOfxImageEffectActionBeginSequenceRender) == 0 ||
        strcmp(action, kOfxImageEffectActionEndSequenceRender) == 0 ||
        strcmp(action, kOfxImageEffectActionIsIdentity) == 0) {
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionOpenGLContextAttached) == 0) {
        return actionGLAttached((OfxImageEffectHandle)handle);
    }
    if (strcmp(action, kOfxActionOpenGLContextDetached) == 0) {
        return actionGLDetached((OfxImageEffectHandle)handle);
    }
    if (strcmp(action, kOfxImageEffectActionGetClipPreferences) == 0) {
        return actionGetClipPreferences(outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetRegionOfDefinition) == 0) {
        return actionGetRoD(outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetRegionsOfInterest) == 0) {
        return actionGetRoI((OfxImageEffectHandle)handle, inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetOutputColourspace) == 0) {
        return actionGetOutputColourspace(inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionRender) == 0) {
        return actionRenderGL((OfxImageEffectHandle)handle, inArgs);
    }
    return kOfxStatReplyDefault;
}

/* 恒透传变体入口：isIdentity 返回 Source；其余同 CPU 插件。 */
static OfxStatus mainEntryID(const char *action, const void *handle,
                             OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        return actionDescribe(handle, 0);
    }
    if (strcmp(action, kOfxImageEffectActionIsIdentity) == 0) {
        return actionIsIdentity(inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetOutputColourspace) == 0) {
        return actionGetOutputColourspace(inArgs, outArgs);
    }
    return mainEntry(action, handle, inArgs, outArgs);
}

/* ---------- interact 变体（M11 §5）：自定义 UI 宿主验证 ---------- */

/* interact 调用记录：追加到 OAK_TEST_PLUGIN_INTERACT_MARKER 指向的文件
 * （宿主测试断言用；未设环境变量时静默）。 */
static void interact_record(const char *fmt, ...)
{
    const char *marker = marker_env("OAK_TEST_PLUGIN_INTERACT_MARKER");
    if (!marker)
        return;
    FILE *f = fopen(marker, "a");
    if (!f)
        return;
    va_list ap;
    va_start(ap, fmt);
    vfprintf(f, fmt, ap);
    va_end(ap);
    fclose(f);
}

/* 记录 pen 事件的真实参数（inArgs 的 canonical/视口/压力属性）。 */
static void record_pen(const char *action, OfxPropertySetHandle inArgs)
{
    int vp[2] = { 0, 0 };
    double canon[2] = { 0.0, 0.0 };
    double pressure = 0.0;
    g_propSuite->propGetIntN(inArgs, kOfxInteractPropPenViewportPosition, 2, vp);
    g_propSuite->propGetDoubleN(inArgs, kOfxInteractPropPenPosition, 2, canon);
    g_propSuite->propGetDouble(inArgs, kOfxInteractPropPenPressure, 0, &pressure);
    interact_record("%s vp=%d,%d canon=%g,%g pressure=%g\n", action, vp[0], vp[1], canon[0],
                    canon[1], pressure);
}

/* 记录 key 事件的真实参数（keySym/keyString）。 */
static void record_key(const char *action, OfxPropertySetHandle inArgs)
{
    int sym = 0;
    char *s = NULL;
    g_propSuite->propGetInt(inArgs, kOfxPropKeySym, 0, &sym);
    g_propSuite->propGetString(inArgs, kOfxPropKeyString, 0, &s);
    interact_record("%s sym=%d str=%s\n", action, sym, s ? s : "");
}

/* kOfxInteractActionDraw：读 inArgs（viewport/pixelScale/time/
 * backgroundImage/drawContext），经 Draw suite setColour + 原生 GL 画
 * 已知色块（矩形 10..30 canonical）。macOS 上真实绘制；非 macOS 跳过
 * GL。返回 OK。 */
static OfxStatus actionInteractDraw(OfxPropertySetHandle inArgs)
{
    double vp[2] = { 0.0, 0.0 };
    double scale[2] = { 1.0, 1.0 };
    double t = 0.0;
    void *bg = NULL;
    void *drawCtx = NULL;
    g_propSuite->propGetDoubleN(inArgs, kOfxInteractPropViewportSize, 2, vp);
    g_propSuite->propGetDoubleN(inArgs, kOfxInteractPropPixelScale, 2, scale);
    g_propSuite->propGetDouble(inArgs, kOfxPropTime, 0, &t);
    g_propSuite->propGetPointer(inArgs, kOfxInteractPropBackgroundImage, 0, &bg);
    g_propSuite->propGetPointer(inArgs, kOfxInteractPropDrawContext, 0, &drawCtx);

    /* Draw suite 状态真实读写：setColour 写、getColour 读（宿主色板）。 */
    double setcol[4] = { 0.0, 0.0, 0.0, 0.0 };
    double getcol[4] = { 0.0, 0.0, 0.0, 0.0 };
    int setcol_st = kOfxStatFailed, getcol_st = kOfxStatFailed;
    if (g_drawSuite && drawCtx) {
        OfxRGBAColourF col = { 0.9f, 0.1f, 0.2f, 1.0f };
        setcol_st = g_drawSuite->setColour(drawCtx, &col);
        setcol[0] = col.r;
        setcol[1] = col.g;
        setcol[2] = col.b;
        setcol[3] = col.a;
        OfxRGBAColourF bgc = { 0.0f, 0.0f, 0.0f, 0.0f };
        getcol_st = g_drawSuite->getColour(drawCtx, kOfxStandardColourOverlayBackground, &bgc);
        getcol[0] = bgc.r;
        getcol[1] = bgc.g;
        getcol[2] = bgc.b;
        getcol[3] = bgc.a;
    }

    int draw_st = kOfxStatFailed;
#ifdef __APPLE__
    /* 清成暗背景 + Draw suite 画实心矩形（canonical 坐标；宿主经正交
     * 投影映射到视口）。 */
    glClearColor(0.05f, 0.05f, 0.05f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (g_drawSuite && drawCtx) {
        OfxPointD pts[2] = { { 10.0, 10.0 }, { 30.0, 30.0 } };
        draw_st = g_drawSuite->draw(drawCtx, kOfxDrawPrimitiveRectangle, pts, 2);
    }
#else
    (void)0;
#endif

    interact_record("draw vp=%gx%g scale=%gx%g t=%g bg=%p setcol_st=%d setcol=%g,%g,%g,%g "
                    "getcol_st=%d getcol=%g,%g,%g,%g draw_st=%d\n",
                    vp[0], vp[1], scale[0], scale[1], t, bg, setcol_st, setcol[0], setcol[1],
                    setcol[2], setcol[3], getcol_st, getcol[0], getcol[1], getcol[2], getcol[3],
                    draw_st);
    return kOfxStatOK;
}

/* interact 入口（经 kOfxImageEffectPluginPropOverlayInteractV2 声明）：
 * 只处理 interact 的 action；效果侧 action 一律 ReplyDefault（不会被
 * 宿主以效果 handle 调到这里）。 */
static OfxStatus mainEntryInteract(const char *action, const void *handle,
                                   OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    (void)handle;
    (void)outArgs;
    if (strcmp(action, kOfxActionNewInteract) == 0) {
        interact_record("new_interact\n");
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionDescribe) == 0) {
        interact_record("describe\n");
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionCreateInstance) == 0) {
        interact_record("create\n");
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionDestroyInstance) == 0) {
        interact_record("destroy\n");
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionDraw) == 0) {
        return actionInteractDraw(inArgs);
    }
    if (strcmp(action, kOfxInteractActionPenMotion) == 0) {
        record_pen("pen_motion", inArgs);
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionPenDown) == 0) {
        record_pen("pen_down", inArgs);
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionPenUp) == 0) {
        record_pen("pen_up", inArgs);
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionKeyDown) == 0) {
        int sym = 0;
        g_propSuite->propGetInt(inArgs, kOfxPropKeySym, 0, &sym);
        record_key("key_down", inArgs);
        /* Escape → ReplyDefault（宿主应透传该状态：插件未处理，事件
         * 可交视图中的其他对象）。 */
        if (sym == kOfxKey_Escape)
            return kOfxStatReplyDefault;
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionKeyUp) == 0) {
        record_key("key_up", inArgs);
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionIdle) == 0) {
        interact_record("idle\n");
        return kOfxStatOK;
    }
    /* GainFocus/LoseFocus 等：默认（ReplyDefault = 未处理，宿主可继续）。 */
    return kOfxStatReplyDefault;
}

/* interact 变体的效果侧 main entry：describe 建参数/clip 并声明 overlay
 * interact V2 入口（指向 mainEntryInteract）；其余效果 action 委托 base
 * mainEntry。 */
static OfxStatus mainEntryInteractEffect(const char *action, const void *handle,
                                         OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        OfxStatus st = actionDescribe(handle, 0);
        if (st != kOfxStatOK)
            return st;
        return propSetPointer(handle, kOfxImageEffectPluginPropOverlayInteractV2, 0,
                              (void *)mainEntryInteract);
    }
    return mainEntry(action, handle, inArgs, outArgs);
}

/* ---------- 导出 ---------- */

static const OfxPlugin test_plugin = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.oak.test-plugin",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntry,
};

static const OfxPlugin test_plugin_gl = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.oak.test-plugin.gl",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntryGL,
};

static const OfxPlugin test_plugin_id = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.oak.test-plugin.identity",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntryID,
};

static const OfxPlugin test_plugin_interact = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.oak.test-plugin.interact",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntryInteractEffect,
};

/* M3 取消 e2e：慢速 filter（progressUpdate x20ms 循环，取消即中止）。 */
static const OfxPlugin test_plugin_slow = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.oak.test-plugin.slow",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntrySlow,
};

OfxExport int OfxGetNumberOfPlugins(void)
{
    return 5;
}

OfxExport OfxPlugin *OfxGetPlugin(int nth)
{
    if (nth == 0)
        return (OfxPlugin *)&test_plugin;
    if (nth == 1)
        return (OfxPlugin *)&test_plugin_gl;
    if (nth == 2)
        return (OfxPlugin *)&test_plugin_id;
    if (nth == 3)
        return (OfxPlugin *)&test_plugin_interact;
    if (nth == 4)
        return (OfxPlugin *)&test_plugin_slow;
    return NULL;
}
