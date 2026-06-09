{
  "targets": [
    {
      "target_name": "mpv",
      "sources": ["src/mpv_addon.cc"],
      "include_dirs": ["<!@(node -p \"require('node-addon-api').include\")"],
      "defines": ["NAPI_VERSION=8"],
      "conditions": [
        ["OS=='win'", {
          # Windows: link the libmpv dev package placed in native/mpv-dev by
          # scripts/setup-windows.ps1 (headers + generated mpv.lib import library).
          "include_dirs": ["<(module_root_dir)/mpv-dev/include"],
          "libraries": ["<(module_root_dir)/mpv-dev/mpv.lib"],
          "msvs_settings": {
            "VCCLCompilerTool": {
              "ExceptionHandling": 1,
              "AdditionalOptions": ["/std:c++17"]
            }
          }
        }, {
          # Linux / macOS: resolve libmpv via pkg-config.
          "cflags!": ["-fno-exceptions"],
          "cflags_cc!": ["-fno-exceptions"],
          "cflags": ["<!@(pkg-config --cflags mpv)"],
          "cflags_cc": ["<!@(pkg-config --cflags mpv)", "-std=c++17"],
          "libraries": ["<!@(pkg-config --libs mpv)"]
        }]
      ]
    }
  ]
}
