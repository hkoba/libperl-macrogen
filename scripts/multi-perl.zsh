#!/bin/zsh
# multi-perl.zsh — 公式 perl:X.Y イメージで複数バージョン Perl に対する
# macrogen のスモーク検査を回すホスト側ラッパ。
#
# usage: scripts/multi-perl.zsh [flags] VERSION[-MODE] ... [-- macrogen 追加引数]
#
#   VERSION-MODE は docker タグの読みに合わせる:
#     5.36            -> perl:5.36           (非 threaded = イメージ既定)
#     5.36-threaded   -> perl:5.36-threaded
#   複数指定で逐次実行: scripts/multi-perl.zsh 5.30-threaded 5.36 5.42-threaded
#
# flags:
#   -x                xtrace
#   -n                dry-run (podman コマンドを表示するだけ)
#   --both            各 VERSION を threaded / non-threaded の両モードで
#   --all             フル matrix (5.20〜5.42 偶数 × 両モード)。ディスク見積りを
#                     表示して確認を求める (--force-yes で省略)。デフォルトでは
#                     絶対に発動しない
#   --out DIR         成果物出力先 root (既定: tmp/multi-perl/out)。leg 毎に
#                     <ver>-<mode>/ を作って volume mount する
#   --shell           検査せず準備済みコンテナの対話 shell へ
#   --shell-on-fail   検査失敗時にコンテナ内対話 shell へ (生成物は /out に)
#   --baseline        期待値 seed 採取モード (harness の --baseline)
#   --strict          XFAIL/XPASS も失敗扱い (harness の --strict)
#   --pull            イメージを強制 pull
#   --image-tag TAG   タグ解決を上書き (例: 5.20-threaded-buster)
#   --clean           tmp/multi-perl の使用量を表示して削除確認
#   -- ARGS...        残りは harness 経由で macrogen へ passthrough
#
# 下流 (libperl-sys) ビルド層 --downstream は Phase 5 で追加予定。

emulate -L zsh
set -e

realScriptFn=$(readlink -f $0)
repoDir=${realScriptFn:h:h}
cd $repoDir

ALL_VERSIONS=(5.20 5.22 5.24 5.26 5.28 5.30 5.32 5.34 5.36 5.38 5.40 5.42)
IMAGE_PREFIX=docker.io/library/perl

#========================================
# `--` 以降を passthrough として先に切り離す (zparseopts は -D で `--` 自体を
# 食ってしまい境界が失われるため)
typeset -a passthru
if (( ${@[(I)--]} )); then
    integer dashIx=${@[(i)--]}
    passthru=(${@[dashIx+1,-1]})
    argv=(${@[1,dashIx-1]})
fi

zparseopts -D -K x=o_xtrace n=o_dryrun \
           -both=o_both -all=o_all -force-yes=o_force_yes \
           -out:=o_out \
           -shell=o_shell -shell-on-fail=o_shell_on_fail \
           -baseline=o_baseline -strict=o_strict \
           -pull=o_pull -image-tag:=o_image_tag \
           -clean=o_clean \
           -downstream=o_downstream

if (($#o_xtrace)); then set -x; fi

function x {
    print -r -- '#' ${(@q-)argv}
    if (($#o_dryrun)); then return; fi
    "$@"
}

if (($#o_downstream)); then
    print -u2 "--downstream は未実装です (実装計画の Phase 5 で追加予定)"
    exit 2
fi

#========================================
# container engine (prior art: libperl-rs/runtest-docker.zsh)
engine=(docker)
if [[ -r /etc/os-release ]]; then
    source /etc/os-release
    if [[ $ID == "fedora" ]] && ((VERSION_ID >= 31)); then
        engine=(podman --cgroup-manager=systemd)
    fi
fi

SELINUX=""
if (($+commands[selinuxenabled])) && selinuxenabled; then
    SELINUX=":z"
fi

stateDir=$repoDir/tmp/multi-perl
outRoot=$stateDir/out
if (($#o_out)); then
    outRoot=${o_out[2]#=}
fi

#========================================
if (($#o_clean)); then
    print "== tmp/multi-perl usage =="
    typeset -a stateItems
    stateItems=($stateDir/*(N))
    if ((#stateItems)); then
        du -sh $stateItems
    else
        print "(empty)"
    fi
    print -n "remove rust-home target out downstream-src? [y/N] "
    read -r ans
    if [[ $ans == [yY]* ]]; then
        x rm -rf $stateDir/rust-home $stateDir/target $stateDir/out \
             $stateDir/downstream-src
    fi
    exit 0
fi

#========================================
# leg リストの組み立て
typeset -a legs
if (($#o_all)); then
    print "== --all: フル matrix (${#ALL_VERSIONS} versions x 2 modes) =="
    print "   イメージだけで概算 12〜16GB のディスクを消費します"
    print "   (1 version ≈ 0.9〜1.1GB + threaded 差分、掃除は --clean と podman rmi)"
    if ((! $#o_force_yes)); then
        print -n "続行しますか? [y/N] "
        read -r ans
        [[ $ans == [yY]* ]] || exit 1
    fi
    for v in $ALL_VERSIONS; do
        legs+=($v-non-threaded $v-threaded)
    done
else
    if ((! ARGC)); then
        print -u2 "usage: $0 [flags] VERSION[-MODE] ...  (詳細はスクリプト冒頭コメント)"
        exit 2
    fi
    for spec in "$@"; do
        case $spec in
            (<->.<->-threaded)     legs+=(${spec%-threaded}-threaded) ;;
            (<->.<->-non-threaded) legs+=(${spec%-non-threaded}-non-threaded) ;;
            (<->.<->)
                if (($#o_both)); then
                    legs+=($spec-non-threaded $spec-threaded)
                else
                    # 裸の VERSION は docker タグの既定に合わせ非 threaded
                    legs+=($spec-non-threaded)
                fi ;;
            (*)
                print -u2 "cannot parse version spec: $spec (expected X.Y[-threaded|-non-threaded])"
                exit 2 ;;
        esac
    done
    if (($#o_both)); then
        # X.Y-threaded 等の明示指定と --both の併用: 明示分はそのまま
        typeset -aU legs
    fi
fi

#========================================
# イメージタグ解決 (fallback 連鎖)。結果は stdout、経過は stderr
function resolve_image {
    local ver=$1 mode=$2
    local -a candidates
    local suffix=""
    [[ $mode == threaded ]] && suffix=-threaded
    if (($#o_image_tag)); then
        candidates=(${o_image_tag[2]#=})
    elif [[ ${ver#5.} -le 26 ]]; then
        # 5.20〜5.26 の素タグは stretch ベースで、glibc ヘッダが古すぎて
        # macrogen の preprocess が通らない (__intN_t 未展開等)。buster を優先
        candidates=($ver$suffix-buster $ver$suffix)
    else
        candidates=($ver$suffix $ver$suffix-buster)
    fi
    local tag image
    for tag in $candidates; do
        image=$IMAGE_PREFIX:$tag
        if ((! $#o_pull)) && $engine image exists $image 2>/dev/null; then
            print -- $image; return 0
        fi
        if x $engine pull $image >&2; then
            print -- $image; return 0
        fi
    done
    print -u2 "FATAL: no usable image for $ver ($mode); tried: $candidates"
    print -u2 "       (tag 一覧: https://hub.docker.com/_/perl — --image-tag で明示指定可)"
    return 1
}

function run_leg {
    local leg=$1; shift
    local ver mode
    if [[ $leg == *-non-threaded ]]; then
        ver=${leg%-non-threaded}; mode=non-threaded
    else
        ver=${leg%-threaded}; mode=threaded
    fi

    local image
    image=$(resolve_image $ver $mode) || return 1

    local outLeg=$outRoot/$leg
    x mkdir -p $stateDir/rust-home $stateDir/target $outLeg

    local -a tty_opt
    if [[ -t 0 && -t 1 ]]; then tty_opt=(-it); fi

    local subcmd=smoke
    if (($#o_shell)); then subcmd=shell; fi

    x $engine run --rm $tty_opt \
      -v $repoDir:/app$SELINUX \
      -v $stateDir/rust-home:/opt/rust$SELINUX \
      -v $stateDir/target:/cargo-target-root$SELINUX \
      -v $outLeg:/out$SELINUX \
      -e MP_LEG=$leg \
      -e MP_SHELL_ON_FAIL=$(($#o_shell_on_fail > 0)) \
      -e MP_BASELINE=$(($#o_baseline > 0)) \
      -e MP_STRICT=$(($#o_strict > 0)) \
      $image bash /app/scripts/multi-perl-inner.sh $subcmd "$@"
}

#========================================
typeset -A leg_status
integer anyFailed=0
for leg in $legs; do
    print "===== leg: $leg ====="
    if run_leg $leg $passthru; then
        leg_status[$leg]=OK
    else
        leg_status[$leg]=NG
        anyFailed=1
    fi
done

print
print "===== multi-perl summary ====="
for leg in $legs; do
    printf "  %-22s %s\n" $leg $leg_status[$leg]
done
if ((anyFailed)); then
    print "results under: $outRoot/<leg>/"
fi
exit $anyFailed
