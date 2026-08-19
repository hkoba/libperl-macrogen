#!/bin/bash
# multi-perl-inner.sh — perl:X.Y コンテナ内で実行される glue。
# scripts/multi-perl.zsh から起動される (直接手で叩いても動く)。
#
# usage: multi-perl-inner.sh {smoke|shell} [-- macrogen への追加引数...]
#
# 期待するマウント:
#   /app               macrogen working tree
#   /opt/rust          共有 RUSTUP_HOME + CARGO_HOME (全コンテナ共通、初回のみ rustup install)
#   /cargo-target-root macrogen の CARGO_TARGET_DIR 置き場 (Debian codename 別に共有)
#   /out               この leg の成果物出力先
# 環境変数 (wrapper が設定):
#   MP_LEG           例 5.36-non-threaded (--require に渡す)
#   MP_SHELL_ON_FAIL 1 なら検査失敗時に対話 shell へ
#   MP_BASELINE      1 なら --baseline
#   MP_STRICT        1 なら --strict
#
# スモーク経路は apt を使わない (curl/gcc は buildpack-deps に同梱)。
# これにより apt リポジトリが EOL の古いイメージ (buster 等) でも素で動く。

set -u

subcmd=${1:-smoke}
shift || true

export RUSTUP_HOME=/opt/rust/rustup
export CARGO_HOME=/opt/rust/cargo
export PATH=$CARGO_HOME/bin:$PATH

# ── rust toolchain (共有 volume に 1 回だけ install) ──
if ! command -v cargo >/dev/null 2>&1; then
    echo "== multi-perl-inner: installing rust toolchain into /opt/rust (first run) =="
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal --component rustfmt \
        || { echo "FATAL: rustup install failed (old base image + TLS?" \
                  " — run a modern leg like 5.42 first to seed /opt/rust)"; exit 1; }
fi

# ── target dir は Debian codename 別に共有 ──
# macrogen バイナリは perl 非依存だが glibc 依存: bookworm でビルドした
# バイナリは buster では動かないため、base 別に分ける (5.20〜5.32 は
# buster を共有し再ビルドなし)
codename=unknown
[ -r /etc/os-release ] && codename=$(. /etc/os-release; echo "${VERSION_CODENAME:-unknown}")
export CARGO_TARGET_DIR=/cargo-target-root/$codename

cd /app || exit 1

echo "== multi-perl-inner: perl=$(perl -e 'print $]') codename=$codename leg=${MP_LEG:-unset} =="

cargo build || exit 1

# apidoc cache は leg 毎に隔離 (cache key が APIDOC_DATA_VERSION のみのため、
# apidoc データ編集中の leg 間汚染を防ぐ。成果物としても残る)
export LIBPERL_APIDOC_CACHE_DIR=/out/apidoc-cache

case $subcmd in
    shell)
        echo "== multi-perl-inner: interactive shell (workspace /app, out /out) =="
        exec bash -l
        ;;
    smoke)
        args=(--require "${MP_LEG:?MP_LEG not set}" --out /out)
        [ "${MP_BASELINE:-0}" = 1 ] && args+=(--baseline)
        [ "${MP_STRICT:-0}" = 1 ] && args+=(--strict)
        [ $# -gt 0 ] && args+=(-- "$@")
        perl scripts/multi-perl-smoke.pl "${args[@]}"
        st=$?
        # tty が無いと exec bash が即終了して失敗が exit 0 に化けるためガード
        if [ $st -ne 0 ] && [ "${MP_SHELL_ON_FAIL:-0}" = 1 ] && [ -t 0 ]; then
            echo
            echo "== smoke FAILED (exit $st) — interactive shell =="
            echo "   workspace: /app   results: /out (host: tmp/multi-perl/out/${MP_LEG:-})"
            echo "   再実行: perl scripts/multi-perl-smoke.pl --require ${MP_LEG:-} --out /out"
            exec bash -l
        fi
        exit $st
        ;;
    *)
        echo "unknown subcommand: $subcmd (expected smoke|shell)" >&2
        exit 2
        ;;
esac
