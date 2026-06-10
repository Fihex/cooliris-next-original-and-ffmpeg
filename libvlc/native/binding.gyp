{
  "targets": [
    {
      "target_name": "vlc",
      "sources": ["src/vlc_addon.cc"],
      "include_dirs": ["<!@(node -p \"require('node-addon-api').include\")"],
      "defines": ["NAPI_VERSION=8"],
      "conditions": [
        ["OS=='win'", {
          # Windows: link the official VLC SDK placed in native/vlc-sdk by
          # scripts/setup-windows.ps1 (sdk/include + sdk/lib from the VLC zip).
          "include_dirs": ["<(module_root_dir)/vlc-sdk/include"],
          "libraries": ["<(module_root_dir)/vlc-sdk/lib/libvlc.lib"],
          "msvs_settings": {
            "VCCLCompilerTool": {
              "ExceptionHandling": 1,
              "AdditionalOptions": ["/std:c++17"]
            }
          }
        }, {
          # Linux / macOS: resolve libvlc via pkg-config.
          "cflags!": ["-fno-exceptions"],
          "cflags_cc!": ["-fno-exceptions"],
          "cflags": ["<!@(pkg-config --cflags libvlc)"],
          "cflags_cc": ["<!@(pkg-config --cflags libvlc)", "-std=c++17"],
          "libraries": ["<!@(pkg-config --libs libvlc)"]
        }]
      ]
    }
  ]
}
