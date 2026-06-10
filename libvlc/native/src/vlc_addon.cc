// Native libVLC binding for the libvlc edition. Exposes the SAME interface the libmpv
// addon had — command(), set/getProperty() (mpv property names), videoSize(),
// renderFrame() — implemented as a shim over libVLC 3.x, so the host script, the IPC
// proxy, and the player UI carry over unchanged.
//
// Video goes through the vmem callbacks (libvlc_video_set_callbacks): VLC decodes and
// scales each frame into an RGBA buffer we own (capped at 1280 wide to bound the
// per-frame IPC); renderFrame() hands a copy to JS for the <canvas>. Audio output and
// subtitle compositing are handled by VLC itself.
#include <napi.h>
#include <vlc/vlc.h>
#include <atomic>
#include <cstdarg>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>

namespace {
constexpr unsigned MAX_WIDTH = 1920; // cap render width (bounds frame IPC size)

uint8_t* alignedPtr(std::vector<uint8_t>& v, size_t size) {
  v.resize(size + 32);
  auto addr = reinterpret_cast<uintptr_t>(v.data());
  return reinterpret_cast<uint8_t*>((addr + 31) & ~uintptr_t(31));
}

void logCb(void*, int level, const libvlc_log_t*, const char* fmt, va_list args) {
  if (level < LIBVLC_WARNING) return; // warnings + errors only (info is very chatty)
  fprintf(stderr, "[vlc:%s] ", level >= LIBVLC_ERROR ? "error" : "warn");
  vfprintf(stderr, fmt, args);
  fputc('\n', stderr);
}
}  // namespace

class VlcPlayer : public Napi::ObjectWrap<VlcPlayer> {
 public:
  static Napi::Object Init(Napi::Env env, Napi::Object exports) {
    Napi::Function func = DefineClass(env, "VlcPlayer", {
      InstanceMethod("command", &VlcPlayer::Command),
      InstanceMethod("setProperty", &VlcPlayer::SetProperty),
      InstanceMethod("getProperty", &VlcPlayer::GetProperty),
      InstanceMethod("renderFrame", &VlcPlayer::RenderFrame),
      InstanceMethod("videoSize", &VlcPlayer::VideoSize),
      InstanceMethod("destroy", &VlcPlayer::Destroy),
    });
    exports.Set("VlcPlayer", func);
    exports.Set("apiVersion", Napi::Function::New(env, ApiVersion));
    return exports;
  }

  VlcPlayer(const Napi::CallbackInfo& info) : Napi::ObjectWrap<VlcPlayer>(info) {
    Napi::Env env = info.Env();
    // Optional ctor arg: an array of VLC command-line options. This is how subtitle
    // style is applied on libVLC 3 (e.g. --freetype-fontsize=…) — these options are
    // creation-time only, so a style change means recreating the player (host does it).
    std::vector<std::string> argStore;
    std::vector<const char*> argv;
    if (info.Length() > 0 && info[0].IsArray()) {
      Napi::Array arr = info[0].As<Napi::Array>();
      for (uint32_t i = 0; i < arr.Length(); i++)
        argStore.push_back(arr.Get(i).ToString().Utf8Value());
      for (auto& s : argStore) argv.push_back(s.c_str());
    }
    inst_ = libvlc_new((int)argv.size(), argv.empty() ? nullptr : argv.data());
    if (!inst_) {
      Napi::Error::New(env, "libvlc_new failed (bad option or missing VLC plugin path?)")
          .ThrowAsJavaScriptException();
      return;
    }
    libvlc_log_set(inst_, logCb, nullptr);
    mp_ = libvlc_media_player_new(inst_);
    if (!mp_) {
      Cleanup();
      Napi::Error::New(env, "libvlc_media_player_new failed").ThrowAsJavaScriptException();
      return;
    }
    // vmem: VLC decodes into our buffer. Format negotiated per-video in SetupCb.
    libvlc_video_set_format_callbacks(mp_, SetupCb, CleanupCb);
    libvlc_video_set_callbacks(mp_, LockCb, UnlockCb, nullptr, this);
  }

  ~VlcPlayer() { Cleanup(); }

 private:
  libvlc_instance_t* inst_ = nullptr;
  libvlc_media_player_t* mp_ = nullptr;

  // Frame plumbing (VLC decode thread ⇄ Node main thread).
  std::mutex mtx_;
  std::vector<uint8_t> backStore_, frontStore_;
  uint8_t* back_ = nullptr;
  uint8_t* front_ = nullptr;
  unsigned vw_ = 0, vh_ = 0, pitch_ = 0;
  std::atomic<bool> haveFrame_{false};
  // Render target width (set from JS to ~display width). VLC scales the video AND
  // renders subtitles at this size, so text stays crisp instead of being upscaled
  // from the source resolution. 0 = just cap the source at MAX_WIDTH.
  std::atomic<unsigned> desiredW_{0};

  // Cached track list (built lazily from VLC track descriptions).
  struct Track {
    std::string type; // "audio" | "sub"
    int id;
    std::string title;
  };
  std::vector<Track> tracks_;

  void Cleanup() {
    if (mp_) {
      libvlc_media_player_stop(mp_);
      libvlc_media_player_release(mp_);
      mp_ = nullptr;
    }
    if (inst_) {
      libvlc_release(inst_);
      inst_ = nullptr;
    }
  }

  /* ------------------------------ vmem callbacks ------------------------------ */

  static unsigned SetupCb(void** opaque, char* chroma, unsigned* width, unsigned* height,
                          unsigned* pitches, unsigned* lines) {
    auto* self = static_cast<VlcPlayer*>(*opaque);
    // Pick the render size: the JS-requested display width when set (up or down —
    // subtitles get rasterised at this size), else the source width; always capped.
    unsigned srcW = *width, srcH = *height;
    unsigned w = self->desiredW_ > 0 ? self->desiredW_.load() : srcW;
    if (w > MAX_WIDTH) w = MAX_WIDTH;
    if (w < 320) w = 320;
    unsigned h = (unsigned)((uint64_t)srcH * w / (srcW ? srcW : 1));
    w &= ~1u;
    h &= ~1u;
    if (!w || !h) return 0;
    memcpy(chroma, "RGBA", 4);
    *width = w;
    *height = h;
    unsigned pitch = (w * 4 + 31) & ~31u;
    pitches[0] = pitch;
    lines[0] = (h + 31) & ~31u;

    std::lock_guard<std::mutex> g(self->mtx_);
    size_t size = (size_t)pitch * lines[0];
    self->back_ = alignedPtr(self->backStore_, size);
    self->front_ = alignedPtr(self->frontStore_, size);
    self->vw_ = w;
    self->vh_ = h;
    self->pitch_ = pitch;
    self->haveFrame_ = false;
    return 1;
  }

  static void CleanupCb(void* opaque) {
    auto* self = static_cast<VlcPlayer*>(opaque);
    std::lock_guard<std::mutex> g(self->mtx_);
    self->haveFrame_ = false;
  }

  static void* LockCb(void* opaque, void** planes) {
    auto* self = static_cast<VlcPlayer*>(opaque);
    planes[0] = self->back_;
    return nullptr;
  }

  static void UnlockCb(void* opaque, void*, void* const*) {
    auto* self = static_cast<VlcPlayer*>(opaque);
    std::lock_guard<std::mutex> g(self->mtx_);
    if (self->back_ && self->front_)
      memcpy(self->front_, self->back_, (size_t)self->pitch_ * self->vh_);
    self->haveFrame_ = true;
  }

  /* ----------------------------- control / playback ---------------------------- */

  bool Load(const std::string& path) {
    if (!inst_ || !mp_) return false;
    libvlc_media_t* media = libvlc_media_new_path(inst_, path.c_str());
    if (!media) return false;
    {
      std::lock_guard<std::mutex> g(mtx_);
      vw_ = vh_ = 0;
      haveFrame_ = false;
      tracks_.clear();
    }
    libvlc_media_player_set_media(mp_, media);
    libvlc_media_release(media);
    return libvlc_media_player_play(mp_) == 0;
  }

  void RefreshTracks() {
    tracks_.clear();
    if (!mp_) return;
    for (auto* d = libvlc_audio_get_track_description(mp_); d; d = d->p_next) {
      if (d->i_id < 0) continue; // skip the "Disable" entry
      tracks_.push_back({"audio", d->i_id, d->psz_name ? d->psz_name : ""});
    }
    for (auto* d = libvlc_video_get_spu_description(mp_); d; d = d->p_next) {
      if (d->i_id < 0) continue;
      tracks_.push_back({"sub", d->i_id, d->psz_name ? d->psz_name : ""});
    }
  }

  static Napi::Value ApiVersion(const Napi::CallbackInfo& info) {
    return Napi::String::New(info.Env(), libvlc_get_version());
  }

  // command(["loadfile", path] | ["cycle","pause"] | ["seek", s, "absolute"|"relative"]
  //         | ["stop"]) — the subset the host/UI actually uses, in mpv shape.
  Napi::Value Command(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!mp_ || !info[0].IsArray()) return Napi::Boolean::New(env, false);
    Napi::Array arr = info[0].As<Napi::Array>();
    std::vector<std::string> a;
    for (uint32_t i = 0; i < arr.Length(); i++) a.push_back(arr.Get(i).ToString().Utf8Value());
    if (a.empty()) return Napi::Boolean::New(env, false);

    bool ok = true;
    if (a[0] == "loadfile" && a.size() >= 2) {
      ok = Load(a[1]);
    } else if (a[0] == "cycle" && a.size() >= 2 && a[1] == "pause") {
      libvlc_media_player_pause(mp_); // toggles
    } else if (a[0] == "seek" && a.size() >= 2) {
      double sec = atof(a[1].c_str());
      libvlc_time_t target =
          (a.size() >= 3 && a[2] == "absolute")
              ? (libvlc_time_t)(sec * 1000.0)
              : libvlc_media_player_get_time(mp_) + (libvlc_time_t)(sec * 1000.0);
      if (target < 0) target = 0;
      libvlc_media_player_set_time(mp_, target);
    } else if (a[0] == "stop") {
      libvlc_media_player_stop(mp_);
    } else if (a[0] == "set" && a.size() >= 3) {
      ok = SetProp(a[1], a[2]);
    } else {
      ok = false;
    }
    return Napi::Boolean::New(env, ok);
  }

  bool SetProp(const std::string& name, const std::string& val) {
    if (!mp_) return false;
    if (name == "pause") {
      libvlc_media_player_set_pause(mp_, val == "yes" ? 1 : 0);
      return true;
    }
    if (name == "volume") {
      int v = atoi(val.c_str());
      if (v < 0) v = 0;
      if (v > 100) v = 100;
      return libvlc_audio_set_volume(mp_, v) == 0;
    }
    if (name == "render-width") {
      // Takes effect at the next playback setup (set before loadfile).
      int w = atoi(val.c_str());
      desiredW_ = w > 0 ? (unsigned)w : 0u;
      return true;
    }
    if (name == "aid") return libvlc_audio_set_track(mp_, atoi(val.c_str())) == 0;
    if (name == "sid") {
      return libvlc_video_set_spu(mp_, val == "no" ? -1 : atoi(val.c_str())) == 0;
    }
    // mpv's sub-style properties (font size, colors, border) have no libVLC 3 runtime
    // equivalent — accept and ignore so callers don't error. (Subtitle styling on the
    // libVLC engine is selection-only.)
    if (name.rfind("sub-", 0) == 0) return true;
    return false;
  }

  Napi::Value SetProperty(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!mp_ || !info[0].IsString()) return Napi::Boolean::New(env, false);
    return Napi::Boolean::New(
        env, SetProp(info[0].As<Napi::String>(), info[1].ToString().Utf8Value()));
  }

  Napi::Value GetProperty(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!mp_ || !info[0].IsString()) return env.Null();
    std::string name = info[0].As<Napi::String>();
    char buf[64];

    if (name == "time-pos") {
      snprintf(buf, sizeof buf, "%.3f", libvlc_media_player_get_time(mp_) / 1000.0);
      return Napi::String::New(env, buf);
    }
    if (name == "duration") {
      snprintf(buf, sizeof buf, "%.3f", libvlc_media_player_get_length(mp_) / 1000.0);
      return Napi::String::New(env, buf);
    }
    if (name == "pause")
      return Napi::String::New(env, libvlc_media_player_is_playing(mp_) ? "no" : "yes");
    if (name == "volume") {
      snprintf(buf, sizeof buf, "%d", libvlc_audio_get_volume(mp_));
      return Napi::String::New(env, buf);
    }
    if (name == "aid") {
      snprintf(buf, sizeof buf, "%d", libvlc_audio_get_track(mp_));
      return Napi::String::New(env, buf);
    }
    if (name == "sid") {
      int s = libvlc_video_get_spu(mp_);
      if (s < 0) return Napi::String::New(env, "no");
      snprintf(buf, sizeof buf, "%d", s);
      return Napi::String::New(env, buf);
    }
    if (name == "track-list/count") {
      RefreshTracks();
      snprintf(buf, sizeof buf, "%zu", tracks_.size());
      return Napi::String::New(env, buf);
    }
    // track-list/<i>/<field>
    if (name.rfind("track-list/", 0) == 0) {
      size_t slash = name.find('/', 11);
      if (slash == std::string::npos) return env.Null();
      size_t idx = (size_t)atoi(name.substr(11, slash - 11).c_str());
      std::string field = name.substr(slash + 1);
      if (idx >= tracks_.size()) return env.Null();
      const Track& t = tracks_[idx];
      if (field == "type") return Napi::String::New(env, t.type);
      if (field == "id") {
        snprintf(buf, sizeof buf, "%d", t.id);
        return Napi::String::New(env, buf);
      }
      if (field == "title" && !t.title.empty()) return Napi::String::New(env, t.title);
      return env.Null(); // lang etc. — VLC descriptions only carry a display name
    }
    return env.Null();
  }

  Napi::Value VideoSize(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    std::lock_guard<std::mutex> g(mtx_);
    Napi::Object o = Napi::Object::New(env);
    o.Set("w", Napi::Number::New(env, haveFrame_ ? (double)vw_ : 0));
    o.Set("h", Napi::Number::New(env, haveFrame_ ? (double)vh_ : 0));
    return o;
  }

  // renderFrame(w, h) → Buffer (w*h*4 RGBA) copied from the latest decoded frame.
  Napi::Value RenderFrame(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    unsigned w = info[0].As<Napi::Number>().Uint32Value();
    unsigned h = info[1].As<Napi::Number>().Uint32Value();
    std::lock_guard<std::mutex> g(mtx_);
    if (!haveFrame_ || !front_ || w != vw_ || h != vh_) return env.Null();
    Napi::Buffer<uint8_t> out = Napi::Buffer<uint8_t>::New(env, (size_t)w * h * 4);
    uint8_t* dst = out.Data();
    for (unsigned y = 0; y < h; y++) {
      memcpy(dst + (size_t)y * w * 4, front_ + (size_t)y * pitch_, (size_t)w * 4);
    }
    for (size_t i = 3, n = (size_t)w * h * 4; i < n; i += 4) dst[i] = 255; // force opaque
    return out;
  }

  Napi::Value Destroy(const Napi::CallbackInfo& info) {
    Cleanup();
    return info.Env().Undefined();
  }
};

Napi::Object InitAll(Napi::Env env, Napi::Object exports) {
  return VlcPlayer::Init(env, exports);
}

NODE_API_MODULE(vlc, InitAll)
