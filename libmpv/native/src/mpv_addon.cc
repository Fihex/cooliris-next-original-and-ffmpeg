// Native libmpv binding (Option C).
//  M1: create/initialise an mpv instance and drive it (command / get / set property).
//  M2: software render API → decoded RGBA frames (video + subtitles already composited
//      by mpv) handed to JS for upload into a texture. No on-screen window; no transcode.
#include <napi.h>
#include <mpv/client.h>
#include <mpv/render.h>
#include <cstdint>  // uint8_t / int64_t — MSVC needs this explicitly (GCC pulls it in)
#include <cstdio>   // fprintf / stderr — likewise (see DrainEvents); else MSVC fails
#include <cstdlib>  // strtoll (parse the window id)
#include <string>
#include <vector>
#ifdef _WIN32
#include <windows.h>  // embed mode: size mpv's --wid child window to fill the parent
#include <cwchar>
#endif
#ifdef _WIN32
#include <windows.h>  // embed mode: size mpv's --wid child window to fill the parent
#include <cwchar>
#endif

class MpvPlayer : public Napi::ObjectWrap<MpvPlayer> {
 public:
  static Napi::Object Init(Napi::Env env, Napi::Object exports) {
    Napi::Function func = DefineClass(env, "MpvPlayer", {
      InstanceMethod("command", &MpvPlayer::Command),
      InstanceMethod("setProperty", &MpvPlayer::SetProperty),
      InstanceMethod("getProperty", &MpvPlayer::GetProperty),
      InstanceMethod("renderFrame", &MpvPlayer::RenderFrame),
      InstanceMethod("videoSize", &MpvPlayer::VideoSize),
      InstanceMethod("fit", &MpvPlayer::Fit),
      InstanceMethod("destroy", &MpvPlayer::Destroy),
    });
    exports.Set("MpvPlayer", func);
    exports.Set("apiVersion", Napi::Function::New(env, ApiVersion));
    return exports;
  }

  MpvPlayer(const Napi::CallbackInfo& info) : Napi::ObjectWrap<MpvPlayer>(info) {
    Napi::Env env = info.Env();
    mpv_ = mpv_create();
    if (!mpv_) {
      Napi::Error::New(env, "mpv_create failed").ThrowAsJavaScriptException();
      return;
    }
    // Embedded-window mode (Option 1): if a native window id is passed, mpv renders
    // DIRECTLY into that window with hardware decode (vo=gpu) — native fps, no per-frame
    // copy. We then skip the SW render context entirely. Used for the lightbox on Windows.
    if (info.Length() > 0 && info[0].IsObject()) {
      Napi::Object cfg = info[0].As<Napi::Object>();
      if (cfg.Has("wid")) {
        std::string widStr = cfg.Get("wid").ToString().Utf8Value();
        int64_t wid = (int64_t)strtoll(widStr.c_str(), nullptr, 10);
        if (wid != 0) {
          embedded_ = true;
          wid_ = wid;
          mpv_set_option(mpv_, "wid", MPV_FORMAT_INT64, &wid);
          mpv_set_option_string(mpv_, "vo", "gpu");
          mpv_set_option_string(mpv_, "hwdec", "auto-safe"); // HW decode, SW fallback
          // Fast start: don't buffer packets in the demuxer before the first frame, and
          // create the GPU window/context up front (idle) so the first open doesn't pay to
          // build the swapchain. (gpu-context auto-picks d3d11 on Windows.)
          mpv_set_option_string(mpv_, "demuxer-lavf-o", "fflags=+nobuffer");
          mpv_set_option_string(mpv_, "force-window", "yes");
          mpv_set_option_string(mpv_, "terminal", "no");
          mpv_set_option_string(mpv_, "idle", "yes");
          // Subtitles auto-show when the video has them (mpv's default sid=auto), in sync
          // from the first frame — no manual enable, no "appears a bit later". sub-auto=all
          // also pulls in sidecar .srt files. (Off only when there are none.)
          mpv_set_option_string(mpv_, "sub-auto", "all");
          mpv_set_option_string(mpv_, "sub-font-size", "44");
          mpv_set_option_string(mpv_, "keep-open", "yes"); // don't close the window at EOF
          mpv_request_log_messages(mpv_, "info");
          if (mpv_initialize(mpv_) < 0) {
            mpv_destroy(mpv_);
            mpv_ = nullptr;
            Napi::Error::New(env, "mpv_initialize (embed) failed").ThrowAsJavaScriptException();
          }
          return;
        }
      }
    }
    // Route video through the libmpv render API (we pull frames), no window.
    mpv_set_option_string(mpv_, "vo", "libmpv");
    mpv_set_option_string(mpv_, "terminal", "no");
    mpv_set_option_string(mpv_, "idle", "yes");
    mpv_set_option_string(mpv_, "sid", "no");          // off by default (but still selectable)
    mpv_set_option_string(mpv_, "sub-auto", "all");  // load every sidecar sub in the folder (any name) into the menu
    mpv_set_option_string(mpv_, "sub-back-color", "#00000000"); // transparent background box
    mpv_set_option_string(mpv_, "sub-border-size", "3");        // keep an outline for readability
    mpv_set_option_string(mpv_, "sub-font-size", "44");
    // Software decode: reliable everywhere. (Hardware decode via the bundled/system
    // libmpv proved flaky — "hardware accelerator failed to decode picture" — and could
    // stall playback; mpv's SW decode is fast enough for this use.)
    mpv_set_option_string(mpv_, "hwdec", "no");
    mpv_request_log_messages(mpv_, "info"); // surfaces ao/codec selection for diagnosis
    if (mpv_initialize(mpv_) < 0) {
      mpv_destroy(mpv_);
      mpv_ = nullptr;
      Napi::Error::New(env, "mpv_initialize failed").ThrowAsJavaScriptException();
      return;
    }
    mpv_render_param params[] = {
      {MPV_RENDER_PARAM_API_TYPE, const_cast<char*>(MPV_RENDER_API_TYPE_SW)},
      {MPV_RENDER_PARAM_INVALID, nullptr},
    };
    if (mpv_render_context_create(&ctx_, mpv_, params) < 0) {
      ctx_ = nullptr;
      Napi::Error::New(env, "mpv_render_context_create failed").ThrowAsJavaScriptException();
    }
  }

  ~MpvPlayer() { Cleanup(); }

 private:
  mpv_handle* mpv_ = nullptr;
  mpv_render_context* ctx_ = nullptr;
  std::vector<uint8_t> buf_;
  bool embedded_ = false; // true → mpv renders into a native window (no SW render context)
  int64_t wid_ = 0;       // the parent window id (embed mode), for sizing mpv's child

  // Embed mode: mpv's --wid surface is a child window that comes up at the video's native
  // size in the corner and doesn't track the parent — size it to fill the parent's client
  // area. Called on video-reconfig and on window resize.
  void FitWindow() {
#ifdef _WIN32
    if (!embedded_ || !wid_) return;
    HWND parent = (HWND)(intptr_t)wid_;
    EnumChildWindows(
        parent,
        [](HWND child, LPARAM lp) -> BOOL {
          wchar_t cls[64] = {0};
          GetClassNameW(child, cls, 63);
          if (wcscmp(cls, L"mpv") == 0) {
            RECT rc;
            GetClientRect((HWND)lp, &rc);
            // Size mpv to fill its parent (the transparent video window). It stays UNDER
            // the web layer so the controls overlay it (the proven embed layering).
            MoveWindow(child, 0, 0, rc.right - rc.left, rc.bottom - rc.top, TRUE);
          }
          return TRUE;
        },
        (LPARAM)parent);
#endif
  }

  // Drain mpv's event queue (it can stall the core if left unread) and surface errors.
  void DrainEvents() {
    if (!mpv_) return;
    for (;;) {
      mpv_event* ev = mpv_wait_event(mpv_, 0);
      if (!ev || ev->event_id == MPV_EVENT_NONE) break;
      if (ev->event_id == MPV_EVENT_LOG_MESSAGE) {
        auto* m = static_cast<mpv_event_log_message*>(ev->data);
        fprintf(stderr, "[mpv:%s] %s", m->level, m->text);
      } else if (ev->event_id == MPV_EVENT_END_FILE) {
        auto* e = static_cast<mpv_event_end_file*>(ev->data);
        if (e->reason == MPV_END_FILE_REASON_ERROR)
          fprintf(stderr, "[mpv] file error: %s\n", mpv_error_string(e->error));
      } else if (ev->event_id == MPV_EVENT_VIDEO_RECONFIG) {
        // The video output (window) was (re)created/resized — fit it to the parent.
        FitWindow();
      }
    }
  }

  void Cleanup() {
    if (ctx_) {
      mpv_render_context_free(ctx_);
      ctx_ = nullptr;
    }
    if (mpv_) {
      mpv_terminate_destroy(mpv_);
      mpv_ = nullptr;
    }
  }

  static Napi::Value ApiVersion(const Napi::CallbackInfo& info) {
    unsigned long v = mpv_client_api_version();
    return Napi::String::New(info.Env(), std::to_string(v >> 16) + "." + std::to_string(v & 0xffff));
  }

  Napi::Value Command(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!mpv_ || !info[0].IsArray()) return Napi::Boolean::New(env, false);
    Napi::Array arr = info[0].As<Napi::Array>();
    std::vector<std::string> hold;
    std::vector<const char*> args;
    for (uint32_t i = 0; i < arr.Length(); i++) hold.push_back(arr.Get(i).ToString().Utf8Value());
    for (auto& s : hold) args.push_back(s.c_str());
    args.push_back(nullptr);
    return Napi::Boolean::New(env, mpv_command(mpv_, args.data()) >= 0);
  }

  Napi::Value SetProperty(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!mpv_ || !info[0].IsString()) return Napi::Boolean::New(env, false);
    std::string name = info[0].As<Napi::String>();
    std::string val = info[1].ToString().Utf8Value();
    return Napi::Boolean::New(env, mpv_set_property_string(mpv_, name.c_str(), val.c_str()) >= 0);
  }

  Napi::Value GetProperty(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!mpv_ || !info[0].IsString()) return env.Null();
    std::string name = info[0].As<Napi::String>();
    char* val = mpv_get_property_string(mpv_, name.c_str());
    if (!val) return env.Null();
    Napi::Value out = Napi::String::New(env, val);
    mpv_free(val);
    return out;
  }

  // videoSize() → { w, h } of the current video (decoded size), or 0×0 if none yet.
  Napi::Value VideoSize(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    DrainEvents();
    int64_t w = 0, h = 0;
    if (mpv_) {
      mpv_get_property(mpv_, "dwidth", MPV_FORMAT_INT64, &w);
      mpv_get_property(mpv_, "dheight", MPV_FORMAT_INT64, &h);
    }
    Napi::Object o = Napi::Object::New(env);
    o.Set("w", Napi::Number::New(env, (double)w));
    o.Set("h", Napi::Number::New(env, (double)h));
    return o;
  }

  // renderFrame(w, h) → Buffer (w*h*4, RGBX) of the current composited frame, or null.
  // The buffer is reused between calls; consume it (upload to a texture) before the next.
  Napi::Value RenderFrame(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!ctx_) return env.Null();
    DrainEvents();
    int w = info[0].As<Napi::Number>().Int32Value();
    int h = info[1].As<Napi::Number>().Int32Value();
    if (w <= 0 || h <= 0) return env.Null();
    size_t stride = (size_t)w * 4;
    buf_.resize(stride * (size_t)h);
    int size[2] = {w, h};
    char fmt[] = "rgb0";
    mpv_render_param rp[] = {
      {MPV_RENDER_PARAM_SW_SIZE, size},
      {MPV_RENDER_PARAM_SW_FORMAT, fmt},
      {MPV_RENDER_PARAM_SW_STRIDE, &stride},
      {MPV_RENDER_PARAM_SW_POINTER, buf_.data()},
      {MPV_RENDER_PARAM_INVALID, nullptr},
    };
    if (mpv_render_context_render(ctx_, rp) < 0) return env.Null();
    // mpv's "rgb0" leaves the 4th byte as 0 → transparent on a canvas. Force opaque.
    for (size_t i = 3; i < buf_.size(); i += 4) buf_[i] = 255;
    // Copy (not an external buffer): Electron's V8 sandbox rejects external buffers
    // over IPC ("External buffers are not allowed").
    return Napi::Buffer<uint8_t>::Copy(env, buf_.data(), buf_.size());
  }

  // Re-fit the embedded video surface to the parent window (call on window resize).
  Napi::Value Fit(const Napi::CallbackInfo& info) {
    FitWindow();
    return info.Env().Undefined();
  }

  Napi::Value Destroy(const Napi::CallbackInfo& info) {
    Cleanup();
    return info.Env().Undefined();
  }
};

Napi::Object InitAll(Napi::Env env, Napi::Object exports) {
  return MpvPlayer::Init(env, exports);
}

NODE_API_MODULE(mpv, InitAll)
