// Native libmpv binding (Option C foundation). Milestone 1: create/initialise an mpv
// instance and drive it (command / get / set property) from Node. Video output via the
// libmpv render API (frames → texture) is added in the next milestone; for now vo=null
// so no window appears.
#include <napi.h>
#include <mpv/client.h>
#include <string>

class MpvPlayer : public Napi::ObjectWrap<MpvPlayer> {
 public:
  static Napi::Object Init(Napi::Env env, Napi::Object exports) {
    Napi::Function func = DefineClass(env, "MpvPlayer", {
      InstanceMethod("command", &MpvPlayer::Command),
      InstanceMethod("setProperty", &MpvPlayer::SetProperty),
      InstanceMethod("getProperty", &MpvPlayer::GetProperty),
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
    // No on-screen window yet (render API comes next); keep audio enabled.
    mpv_set_option_string(mpv_, "vo", "null");
    mpv_set_option_string(mpv_, "terminal", "no");
    mpv_set_option_string(mpv_, "idle", "yes");
    if (mpv_initialize(mpv_) < 0) {
      mpv_destroy(mpv_);
      mpv_ = nullptr;
      Napi::Error::New(env, "mpv_initialize failed").ThrowAsJavaScriptException();
    }
  }

  ~MpvPlayer() { Cleanup(); }

 private:
  mpv_handle* mpv_ = nullptr;

  void Cleanup() {
    if (mpv_) {
      mpv_terminate_destroy(mpv_);
      mpv_ = nullptr;
    }
  }

  static Napi::Value ApiVersion(const Napi::CallbackInfo& info) {
    unsigned long v = mpv_client_api_version();
    return Napi::String::New(info.Env(), std::to_string(v >> 16) + "." + std::to_string(v & 0xffff));
  }

  // command([...string]) → run an mpv command (e.g. ["loadfile", path]).
  Napi::Value Command(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!mpv_ || !info[0].IsArray()) return Napi::Boolean::New(env, false);
    Napi::Array arr = info[0].As<Napi::Array>();
    std::vector<std::string> hold;
    std::vector<const char*> args;
    for (uint32_t i = 0; i < arr.Length(); i++) hold.push_back(arr.Get(i).ToString().Utf8Value());
    for (auto& s : hold) args.push_back(s.c_str());
    args.push_back(nullptr);
    int rc = mpv_command(mpv_, args.data());
    return Napi::Boolean::New(env, rc >= 0);
  }

  // setProperty(name, value) — value coerced to string (mpv parses it).
  Napi::Value SetProperty(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (!mpv_ || !info[0].IsString()) return Napi::Boolean::New(env, false);
    std::string name = info[0].As<Napi::String>();
    std::string val = info[1].ToString().Utf8Value();
    int rc = mpv_set_property_string(mpv_, name.c_str(), val.c_str());
    return Napi::Boolean::New(env, rc >= 0);
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

  Napi::Value Destroy(const Napi::CallbackInfo& info) {
    Cleanup();
    return info.Env().Undefined();
  }
};

Napi::Object InitAll(Napi::Env env, Napi::Object exports) {
  return MpvPlayer::Init(env, exports);
}

NODE_API_MODULE(mpv, InitAll)
