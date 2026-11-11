#!/bin/bash
# Облачное окружение Claude Code для Урфина (решение 2026-11-11, docs/DEV.md «Где что собирается»).
# Ставит заголовки и библиотеки Windows SDK/CRT (xwin), чтобы
#   cargo check --target x86_64-pc-windows-msvc
# собирал C-зависимости (zstd, unrar, tree-sitter, ring) на Linux.
#
# Текст вставляется в поле «Setup script» окружения: выполняется от root до запуска сессии,
# результат кэшируется снимком (пока укладывается примерно в 5 минут).
# Флаг --accept-license ниже — согласие с лицензией Microsoft на эти заголовки и библиотеки;
# его даёт тот, кто вставляет скрипт.
#
# В настройках того же окружения нужны ещё:
#   Network access — Limited, Allowed domains:
#     aka.ms
#     download.visualstudio.microsoft.com
#   Environment variables:
#     CC_x86_64_pc_windows_msvc=/usr/local/bin/xwin-clang-cl
#     CXX_x86_64_pc_windows_msvc=/usr/local/bin/xwin-clang-cl
#     AR_x86_64_pc_windows_msvc=/usr/local/bin/xwin-llvm-lib
set -uo pipefail

XWIN_VERSION=0.10.0
OUT=/opt/xwin
# Тулчейн из rust-toolchain.toml: ставится заранее, чтобы попал в снимок (не критично).
TOOLCHAIN=1.99.0
export PATH="/usr/local/bin:${CARGO_HOME:-$HOME/.cargo}/bin:/usr/local/cargo/bin:$PATH"

log() { echo "[urfin-cloud] $*"; }
fail() { log "ERROR: $*"; exit 1; }
pick() {
  for n in "$@"; do
    if command -v "$n" >/dev/null 2>&1; then command -v "$n"; return 0; fi
  done
  return 1
}

# 1. clang-cl и llvm-lib: в образе обычно есть, иначе ставим.
find_tools() {
  CL=$(pick clang-cl clang-cl-20 clang-cl-19 clang-cl-18 clang-cl-17) || CL=
  LIB=$(pick llvm-lib llvm-lib-20 llvm-lib-19 llvm-lib-18 llvm-lib-17) || LIB=
}
find_tools
if [ -z "$CL" ] || [ -z "$LIB" ]; then
  log "installing clang and llvm"
  apt-get update -y >/dev/null 2>&1
  DEBIAN_FRONTEND=noninteractive apt-get install -y clang clang-tools lld llvm >/dev/null 2>&1
  find_tools
fi
[ -n "$CL" ] || fail "clang-cl not found"
[ -n "$LIB" ] || fail "llvm-lib not found"

# 2. xwin: готовый бинарь, иначе сборка из исходников (дольше — снимок может не закэшироваться).
if ! command -v xwin >/dev/null 2>&1; then
  p="xwin-$XWIN_VERSION-x86_64-unknown-linux-musl"
  if curl -fsSL --retry 2 -o /tmp/xwin.tgz \
      "https://github.com/Jake-Shadle/xwin/releases/download/$XWIN_VERSION/$p.tar.gz" \
    && tar -xzf /tmp/xwin.tgz -C /tmp \
    && install -m 755 "/tmp/$p/xwin" /usr/local/bin/xwin; then
    log "xwin $XWIN_VERSION: prebuilt"
  else
    log "xwin: prebuilt unavailable, building from source"
    cargo install xwin --locked --root /usr/local || fail "xwin not installed"
  fi
  rm -rf /tmp/xwin.tgz "/tmp/$p"
fi

# 3. Заголовки и библиотеки (x86_64, desktop).
if [ ! -d "$OUT/crt/include" ]; then
  xwin --accept-license --cache-dir /tmp/xwin-cache splat --output "$OUT" \
    || fail "xwin splat failed (allowed domains: aka.ms, download.visualstudio.microsoft.com?)"
  rm -rf /tmp/xwin-cache
fi
chmod -R a+rX "$OUT"

# 4. Обёртки: постоянные пути без пробелов для переменных окружения. Имена содержат
#    «clang-cl» и «llvm-lib» — по ним крейт `cc` узнаёт вид компилятора.
cat > /usr/local/bin/xwin-clang-cl <<EOF
#!/bin/sh
exec "$CL" -Wno-unused-command-line-argument -fuse-ld=lld-link \\
  /vctoolsdir "$OUT/crt" /winsdkdir "$OUT/sdk" \\
  /imsvc "$OUT/crt/include" /imsvc "$OUT/sdk/include/ucrt" \\
  /imsvc "$OUT/sdk/include/um" /imsvc "$OUT/sdk/include/shared" "\$@"
EOF
cat > /usr/local/bin/xwin-llvm-lib <<EOF
#!/bin/sh
exec "$LIB" "\$@"
EOF
chmod 755 /usr/local/bin/xwin-clang-cl /usr/local/bin/xwin-llvm-lib

# 5. Тулчейн проекта с целью Windows — в снимок; при сбое rustup поставит его в сессии сам.
rustup toolchain install "$TOOLCHAIN" --profile minimal -c clippy -c rustfmt \
  -t x86_64-pc-windows-msvc >/dev/null 2>&1 || log "toolchain $TOOLCHAIN not preinstalled"

log "done: $(du -sh "$OUT" 2>/dev/null | cut -f1) in $OUT; cl=$CL lib=$LIB"
