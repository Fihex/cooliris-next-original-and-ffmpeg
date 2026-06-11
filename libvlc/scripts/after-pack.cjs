// electron-builder afterPack hook (Windows only).
//
// Pre-builds the libVLC plugin cache (plugins.dat) inside the *packed* vendor\plugins so
// libVLC loads its module bank from the cache instead of rescanning every plugin DLL on
// launch (~30s on Windows). VLC's cache uses paths relative to the plugin dir + each
// plugin's mtime+size, so it is relocatable: at runtime vlc.ts copies the plugins into a
// writable per-user dir PRESERVING TIMESTAMPS, which keeps this cache valid → fast first
// open. (The copy is also what lets libVLC rewrite the cache if it ever needs to.)
const { execFileSync } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");

exports.default = async function afterPack(context) {
  if (context.electronPlatformName !== "win32") return;
  const vendor = path.join(context.appOutDir, "resources", "vendor");
  const gen = path.join(vendor, "vlc-cache-gen.exe");
  const plugins = path.join(vendor, "plugins");
  if (!fs.existsSync(gen) || !fs.existsSync(plugins)) {
    console.log(`::warning::after-pack: vlc-cache-gen.exe or plugins\\ missing in ${vendor}; skipping plugin cache`);
    return;
  }
  try {
    // vlc-cache-gen loads libvlccore.dll from its own dir (vendor\), then writes
    // plugins.dat into the given plugins directory.
    execFileSync(gen, [plugins], { cwd: vendor, stdio: "inherit" });
    const dat = path.join(plugins, "plugins.dat");
    if (fs.existsSync(dat)) {
      console.log(`after-pack: built plugin cache (${fs.statSync(dat).size} bytes) → ${dat}`);
    } else {
      console.log("::warning::after-pack: vlc-cache-gen ran but plugins.dat was not produced");
    }
  } catch (e) {
    console.log(`::warning::after-pack: vlc-cache-gen failed (${e.message}); app will work but start slowly`);
  }
};
