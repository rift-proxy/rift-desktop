#!/bin/sh
# Entry point of build.Dockerfile. Reads a tar of the source tree on stdin, builds the Windows
# installer and leaves the results in /out. The updater signing key and endpoint
# arrive as environment variables or as a read-only /run/rift-build/.env, never in the image.
# Dependency install scripts and downloads run without them: they are given to the build itself.
set -eu

# /out is the host's output directory, mounted for this build only; the host marks it as the
# output of a build before starting the container, so an arbitrary directory is never cleared.
if [ ! -f /out/.rift-build-output ]; then
  echo "refusing to write: /out is not marked as a build output directory" >&2
  exit 1
fi
if [ -z "${RIFT_BUILD_COMMIT:-}" ]; then
  echo "RIFT_BUILD_COMMIT is not set" >&2
  exit 1
fi

without_secrets() {
  env -u RIFT_UPDATE_ENDPOINT \
    -u TAURI_SIGNING_PRIVATE_KEY -u TAURI_SIGNING_PRIVATE_KEY_PASSWORD "$@"
}

# -m: the files get the time of extraction, not the commit's. Cargo decides what to rebuild from
# modification times against the state kept in the target volume; with commit times, building an
# older commit after a newer one would find its sources "older than the build" and reuse the
# newer commit's objects.
tar -x -m -f - -C /work

cd /work/desktop
without_secrets pnpm install --frozen-lockfile

cd /work
# The build must not resolve dependencies again: a Cargo.lock that does not match the manifests
# fails here instead of being quietly rewritten inside a throwaway container. It also fills the
# registry cache for the target, so the build itself downloads nothing.
without_secrets cargo fetch --locked --target x86_64-pc-windows-msvc --manifest-path /work/Cargo.toml
without_secrets node tooling/sidecar.mjs windows

# build.rs reads the endpoint from .env next to Cargo.toml; the file appears only for the build and is
# removed however the build ends.
trap 'rm -f /work/.env' EXIT
if [ -f /run/rift-build/.env ]; then
  cp /run/rift-build/.env /work/.env
fi
release=target/x86_64-pc-windows-msvc/release
# The target volume survives between builds: installers and signatures of earlier builds must not
# be taken for this one.
rm -rf "/work/$release/bundle"
# The interface build leaves its source maps here instead of in dist, which the executable embeds.
export RIFT_MAPS_DIR=/work/desktop/dist-maps
rm -rf "$RIFT_MAPS_DIR"
node tooling/tauri-build.mjs --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
rm -f /work/.env

rm -rf /out/bundle/nsis /out/rift-desktop.exe /out/frontend /out/symbols /out/build-manifest.json
mkdir -p /out/bundle
cp "$release/rift-desktop.exe" /out/rift-desktop.exe
cp -r "$release/bundle/nsis" /out/bundle/nsis
# The frontend inside the executable is compressed; the bundle it was built from is kept beside
# it so that it can be scanned too.
cp -r /work/desktop/dist /out/frontend
# Matching debug files support local investigation of journal errors. Keep them separate from
# the shipped/scanned files; the build manifest leaves them out. Missing symbols fail the build.
mkdir -p /out/symbols
cp "$release/rift_desktop.pdb" /out/symbols/rift_desktop.pdb
cp -r "$RIFT_MAPS_DIR" /out/symbols/maps
# Written last: a directory without it is a build that did not finish.
node tooling/build-manifest.mjs write /out "$RIFT_BUILD_COMMIT"
echo "built: $(ls /out/bundle/nsis | tr '\n' ' ')"
