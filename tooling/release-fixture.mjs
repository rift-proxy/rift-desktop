import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
export function fixture(t) {
  const base = mkdtempSync(path.join(tmpdir(), "rift-release-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const keyId = Buffer.from("0102030405060708", "hex");
  const pk = Buffer.concat([Buffer.from("Ed"), keyId, publicKey.export({ format: "der", type: "spki" }).subarray(-32)]);
  const pubkey = Buffer.from(`untrusted comment: test public key\n${pk.toString("base64")}\n`).toString("base64");
  const config = { version: "1.2.3", plugins: { updater: { pubkey } } };
  mkdirSync(path.join(base, "desktop/src-tauri"), { recursive: true });
  mkdirSync(path.join(base, "fuzz"));
  const put = (name, data) => writeFileSync(path.join(base, name), data);
  put("Cargo.toml", '[workspace.package]\nversion = "1.2.3"\n');
  put("desktop/package.json", '{"version":"1.2.3"}');
  put("desktop/src-tauri/tauri.conf.json", JSON.stringify(config));
  const lock = (name, version) => `[[package]]\nname = "${name}"\nversion = "${version}"\n`;
  put("Cargo.lock", lock("rift-core", "1.2.3") + lock("rift-desktop", "1.2.3"));
  put("fuzz/Cargo.toml", '[package]\nname = "rift-fuzz"\nversion = "0.0.0"\n');
  put("fuzz/Cargo.lock", lock("rift-core", "1.2.3") + lock("rift-fuzz", "0.0.0"));
  const data = Buffer.concat([Buffer.from("MZ"), Buffer.alloc(256, 7)]);
  function signature(content = data, mode = "ED", comment = "timestamp:1234\tfile:Rift_1.2.3_x64-setup.exe\tversion:1.2.3") {
    const payload = mode === "ED" ? createHash("blake2b512").update(content).digest() : content;
    const raw = sign(null, payload, privateKey);
    const packet = Buffer.concat([Buffer.from(mode), keyId, raw]);
    const global = sign(null, Buffer.concat([raw, Buffer.from(comment)]), privateKey);
    return Buffer.from(`untrusted comment: test signature\n${packet.toString("base64")}\ntrusted comment: ${comment}\n${global.toString("base64")}\n`).toString("base64");
  }
  mkdirSync(path.join(base, "bundle"));
  put("bundle/Rift_1.2.3_x64-setup.exe", data);
  put("bundle/Rift_1.2.3_x64-setup.exe.sig", signature());
  return { base, put, data, signature, pubkey, config, bundle: path.join(base, "bundle"), output: path.join(base, "release"), options: { base, repo: "public-owner/rift-desktop" } };
}
