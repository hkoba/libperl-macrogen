#!/bin/bash
# multi-perl-inner.sh — perl:X.Y コンテナ内で実行される glue。
# scripts/multi-perl.tcl から起動される (直接手で叩いても動く)。
#
# usage: multi-perl-inner.sh {smoke|downstream|shell} [-- macrogen への追加引数...]
#
# 期待するマウント:
#   /app               macrogen working tree
#   /opt/rust          共有 RUSTUP_HOME + CARGO_HOME (全コンテナ共通、初回のみ rustup install)
#   /cargo-target-root macrogen の CARGO_TARGET_DIR 置き場 (Debian codename 別に共有)
#   /out               この leg の成果物出力先
#   /downstream        (downstream のみ) libperl-rs の scratch copy
#                      ([patch.crates-io] で libperl-macrogen -> /app 済み)
#   /downstream-target (downstream のみ) leg 別 CARGO_TARGET_DIR
# 環境変数 (wrapper が設定):
#   MP_LEG             例 5.36-non-threaded (--require に渡す)
#   MP_SHELL_ON_FAIL   1 なら検査失敗時に対話 shell へ
#   MP_BASELINE        1 なら --baseline
#   MP_STRICT          1 なら --strict
#   MP_USE_ARCHIVE     auto|yes|no: EOL Debian の apt を archive.debian.org へ
#                      書き換えるか (auto = stretch/buster のとき)
#   MP_DOWNSTREAM_TEST 1 なら downstream で cargo test --workspace も実行
#
# スモーク経路は apt を使わない (curl/gcc は buildpack-deps に同梱)。
# これにより apt リポジトリが EOL の古いイメージ (buster 等) でも素で動く。
# apt が要るのは downstream (bindgen 用 clang/libclang) だけ。

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

# apidoc cache は leg 毎に隔離 (cache key が APIDOC_DATA_VERSION のみのため、
# apidoc データ編集中の leg 間汚染を防ぐ。成果物としても残る)
export LIBPERL_APIDOC_CACHE_DIR=/out/apidoc-cache

case $subcmd in
    shell)
        cargo build || exit 1
        echo "== multi-perl-inner: interactive shell (workspace /app, out /out) =="
        exec bash -l
        ;;
    smoke)
        cargo build || exit 1
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
    downstream)
        # ── apt: bindgen 用の clang/libclang (この層だけ) ──
        use_archive=${MP_USE_ARCHIVE:-auto}
        apt_opts=()
        if [ "$use_archive" = yes ] || { [ "$use_archive" = auto ] && \
             { [ "$codename" = stretch ] || [ "$codename" = buster ]; }; }; then
            echo "== multi-perl-inner: rewriting apt sources to archive.debian.org ($codename) =="
            cat > /etc/apt/sources.list <<EOF
deb http://archive.debian.org/debian/ $codename main
deb http://archive.debian.org/debian-security/ $codename/updates main
EOF
            apt_opts=(-o Acquire::Check-Valid-Until=false)
        fi
        apt-get "${apt_opts[@]}" update || exit 1
        DEBIAN_FRONTEND=noninteractive apt-get install -y \
            llvm-dev libclang-dev clang || exit 1

        export CARGO_TARGET_DIR=/downstream-target
        cd /downstream/libperl-sys || exit 1

        # scratch の Cargo.lock が crates.io 版 macrogen を pin したままだと
        # semver 互換な [patch.crates-io] は適用されない ([[patch.unused]])。
        # patch 先 (path = /app) へ明示的に更新し、適用を検証する
        cargo update -p libperl-macrogen
        if ! cargo tree -p libperl-sys 2>/dev/null | grep -q 'libperl-macrogen v[0-9.]* (/app)'; then
            echo "FATAL: [patch.crates-io] libperl-macrogen -> /app が効いていない" >&2
            cargo tree -p libperl-sys 2>/dev/null | grep libperl-macrogen >&2 || true
            exit 1
        fi

        set -o pipefail
        cargo build 2>&1 | tee /out/build-error.log
        st=$?
        if [ $st -eq 0 ] && [ "${MP_DOWNSTREAM_TEST:-0}" = 1 ]; then
            (cd /downstream && cargo test --workspace 2>&1 | tee /out/downstream-test.log)
            st=$?
        fi
        set +o pipefail

        # ── 成否に関わらず生成物を回収 (vpatches ツールの入力になる) ──
        # 一次抽出の `--> .../out/macro_bindings.rs:` は rustc エラー出力に
        # しか現れない → 成功ビルドでは fallback の find が主経路。build.rs の
        # 再生成で hash dir が変わると旧 dir の stale copy が残るため、
        # mtime 最新の 1 件を選ぶ (head -1 のディレクトリ順は不定)
        gen=$(perl -nle 'm,--> (\S+/out/macro_bindings\.rs):, and print $1 and exit' \
              /out/build-error.log 2>/dev/null || true)
        if [ -z "$gen" ]; then
            gen=$(find "$CARGO_TARGET_DIR"/debug/build -name macro_bindings.rs \
                       -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)
        fi
        if [ -n "$gen" ] && [ -f "$gen" ]; then
            cp -v "$gen" /out/macro_bindings.rs
            [ -f "${gen%/*}/bindings.rs" ] && cp -v "${gen%/*}/bindings.rs" /out/bindings.rs
        else
            echo "== multi-perl-inner: macro_bindings.rs not found (build died before macrogen?) =="
        fi

        if [ $st -ne 0 ] && [ "${MP_SHELL_ON_FAIL:-0}" = 1 ] && [ -t 0 ]; then
            echo
            echo "== downstream FAILED (exit $st) — interactive shell =="
            echo "   downstream tree: /downstream/libperl-sys   results: /out"
            exec bash -l
        fi
        exit $st
        ;;
    *)
        echo "unknown subcommand: $subcmd (expected smoke|downstream|shell)" >&2
        exit 2
        ;;
esac
