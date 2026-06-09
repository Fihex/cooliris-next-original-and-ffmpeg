{
  "targets": [
    {
      "target_name": "mpv",
      "sources": ["src/mpv_addon.cc"],
      "include_dirs": ["<!@(node -p \"require('node-addon-api').include\")"],
      "cflags!": ["-fno-exceptions"],
      "cflags_cc!": ["-fno-exceptions"],
      "cflags": ["<!@(pkg-config --cflags mpv)"],
      "cflags_cc": ["<!@(pkg-config --cflags mpv)", "-std=c++17"],
      "libraries": ["<!@(pkg-config --libs mpv)"],
      "defines": ["NAPI_VERSION=8"]
    }
  ]
}
