import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

const hooks = join(import.meta.dirname, "../desktop/src-tauri/windows/installer-hooks.nsh");
const source = readFileSync(hooks, "utf8");

// Tauri's own section skips the install for any non-empty `pv`; Microsoft's documented rule
// (WebView2 distribution docs, "Detect if a WebView2 Runtime is already installed") is `pv`
// above 0.0.0.0 in HKLM or HKCU. These tests pin the hook that applies the documented rule.
// The registry behavior itself is not run here (it needs makensis and Wine); it is checked by
// hand on a real Windows machine.
test("the hook applies Microsoft's pv rule to both registry hives", () => {
  assert.match(source, /HKLM "SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\\$\{WEBVIEW2APPGUID\}" "pv"/);
  assert.match(source, /HKCU "SOFTWARE\\Microsoft\\EdgeUpdate\\Clients\\\$\{WEBVIEW2APPGUID\}" "pv"/);
  assert.equal(source.match(/\$0 != "0\.0\.0\.0"/g)?.length, 2, "0.0.0.0 is rejected per hive");
  assert.equal(source.match(/\$0 != ""/g)?.length, 2, "an empty value is rejected per hive");
});

test("the hook downloads the Evergreen bootstrapper silently and fails the install on error", () => {
  const hook = source.slice(source.indexOf("!macro NSIS_HOOK_PREINSTALL"));
  assert.match(hook, /\$UpdateMode <> 1/, "updates keep Tauri's behavior");
  assert.match(hook, /NSISdl::download "https:\/\/go\.microsoft\.com\/fwlink\/p\/\?LinkId=2124703"/);
  assert.match(hook, /ExecWait '"\$TEMP\\MicrosoftEdgeWebview2Setup\.exe" \/silent \/install' \$0/);
  assert.equal(hook.match(/Abort "\$\(webview2AbortError\)"/g)?.length, 2, "download and install errors abort");
  assert.doesNotMatch(source, /\bFile\b/, "no runtime installer is embedded in the package");
});

test("the download runs only when the usability check found no runtime", () => {
  const hook = source.slice(source.indexOf("!macro NSIS_HOOK_PREINSTALL"));
  const check = hook.indexOf("!insertmacro RIFT_WEBVIEW2_USABLE $1");
  const gate = hook.indexOf("${If} $1 = 0");
  const download = hook.indexOf("NSISdl::download");
  const run = hook.indexOf("ExecWait");
  assert.ok(check >= 0, "the hook evaluates the pv rule");
  assert.ok(check < gate, "the gate follows the check");
  assert.ok(gate < download && download < run, "download and install sit inside the gate");
});
