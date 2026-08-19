#!/usr/bin/env tclsh
# multi-perl.tcl — 公式 perl:X.Y イメージで複数バージョン Perl に対する
# macrogen のスモーク検査 / 下流 (libperl-sys) ビルドを回すホスト側ラッパ。
# コンテナ内の実処理は scripts/multi-perl-inner.sh、検査本体は
# scripts/multi-perl-smoke.pl (旧 scripts/multi-perl.zsh の Tcl 移植)。
#
# usage: scripts/multi-perl.tcl [flags] VERSION[-MODE] ... [-- macrogen 追加引数]
#
#   VERSION-MODE は docker タグの読みに合わせる:
#     5.36            -> perl:5.36           (非 threaded = イメージ既定)
#     5.36-threaded   -> perl:5.36-threaded
#   複数指定で逐次実行: scripts/multi-perl.tcl 5.30-threaded 5.36 5.42-threaded
#
# flags:
#   -n                dry-run (実行するコマンドを表示するだけ)
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
#   --downstream      スモークに加えて下流 (libperl-sys) ビルド層も実行。
#                     libperl-rs を shallow clone (キャッシュ) → scratch copy に
#                     [patch.crates-io] で macrogen working tree を注入してビルド。
#                     失敗時の build-error.log / macro_bindings.rs は /out に回収
#                     され tools/build-error-to-vpatches.pl の入力になる
#   --downstream-test downstream で cargo test --workspace も実行
#   --libperl-rs DIR  clone の代わりにローカル checkout を使用 (汚さない)
#   --branch BR       clone するブランチ (既定: 12-macrogen-2)
#   --use-debian-archive / --no-debian-archive
#                     EOL Debian の apt 書き換えを強制 / 抑止 (既定: codename 自動)
#   -- ARGS...        残りは harness 経由で macrogen へ passthrough

package require Tcl 8.6-

set ALL_VERSIONS {5.20 5.22 5.24 5.26 5.28 5.30 5.32 5.34 5.36 5.38 5.40 5.42}
set IMAGE_PREFIX docker.io/library/perl
set DOWNSTREAM_URL https://github.com/hkoba/libperl-rs

set repoDir [file dirname [file dirname [file normalize [info script]]]]
cd $repoDir

#========================================
# オプション解析

array set opt {
    dryrun 0 both 0 all 0 force-yes 0 out {} shell 0 shell-on-fail 0
    baseline 0 strict 0 pull 0 image-tag {} clean 0
    downstream 0 downstream-test 0 libperl-rs {} branch 12-macrogen-2
    use-archive auto
}

proc usage {{rc 2}} {
    puts stderr "usage: scripts/multi-perl.tcl \[flags] VERSION\[-MODE] ...\
 (詳細はスクリプト冒頭コメント)"
    exit $rc
}

set specs {}
set passthru {}
for {set i 0} {$i < [llength $argv]} {incr i} {
    set a [lindex $argv $i]
    switch -- $a {
        -n { set opt(dryrun) 1 }
        --both - --all - --force-yes - --shell - --shell-on-fail -
        --baseline - --strict - --pull - --clean -
        --downstream - --downstream-test {
            set opt([string range $a 2 end]) 1
        }
        --out - --image-tag - --libperl-rs - --branch {
            incr i
            if {$i >= [llength $argv]} { puts stderr "$a needs a value"; exit 2 }
            set opt([string range $a 2 end]) [lindex $argv $i]
        }
        --use-debian-archive { set opt(use-archive) yes }
        --no-debian-archive  { set opt(use-archive) no }
        --help - -h { usage 0 }
        -- { set passthru [lrange $argv [expr {$i + 1}] end]; break }
        default {
            if {[string match -* $a]} { puts stderr "unknown option: $a"; exit 2 }
            lappend specs $a
        }
    }
}

#========================================
# 実行ヘルパ

proc shellJoin {words} {
    join [lmap w $words {
        if {[regexp {^[\w@%+=:,./-]+$} $w]} {
            set w
        } else {
            format {'%s'} [string map {' '\''} $w]
        }
    }] " "
}

# コマンドを表示して実行し exit code を返す (dry-run では表示のみ)。
# stdio をそのまま継ぐので podman run -it の対話もそのまま動く
proc runCmd {args} {
    global opt
    puts "# [shellJoin $args]"
    if {$opt(dryrun)} { return 0 }
    if {[catch {exec {*}$args >@stdout 2>@stderr <@stdin} err eopts]} {
        set ec [dict get $eopts -errorcode]
        switch -- [lindex $ec 0] {
            CHILDSTATUS { return [lindex $ec 2] }
            CHILDKILLED { return 1 }
            default { puts stderr "exec error: $err"; return 1 }
        }
    }
    return 0
}

proc haveTty {} {
    # -mode オプションは端末 (serial 系 channel) にしか存在しない
    expr {![catch {chan configure stdin -mode}]}
}

proc confirm {msg} {
    puts -nonewline "$msg \[y/N] "
    flush stdout
    if {[gets stdin ans] < 0} { return 0 }
    string match -nocase y* [string trim $ans]
}

#========================================
# container engine (prior art: libperl-rs/runtest-docker.zsh)

set engine {docker}
if {![catch {open /etc/os-release} fh]} {
    set osRelease [read $fh]; close $fh
    set osId ""; set osVer 0
    regexp -line {^ID="?(\w+)} $osRelease -> osId
    regexp -line {^VERSION_ID="?(\d+)} $osRelease -> osVer
    if {$osId eq "fedora" && $osVer >= 31} {
        set engine {podman --cgroup-manager=systemd}
    }
}

set SELINUX ""
if {[auto_execok selinuxenabled] ne "" && ![catch {exec selinuxenabled}]} {
    set SELINUX ":z"
}

set stateDir [file join $repoDir tmp multi-perl]
set outRoot [file join $stateDir out]
if {$opt(out) ne ""} { set outRoot [file normalize $opt(out)] }

#========================================
if {$opt(clean)} {
    puts "== tmp/multi-perl usage =="
    set items [lsort [glob -nocomplain -directory $stateDir *]]
    if {[llength $items]} {
        catch {exec du -sh {*}$items >@stdout 2>@stderr}
    } else {
        puts "(empty)"
    }
    if {[confirm "remove rust-home target target-downstream out downstream-src?"]} {
        runCmd rm -rf {*}[lmap d {rust-home target target-downstream out downstream-src} {
            file join $stateDir $d
        }]
    }
    exit 0
}

#========================================
# leg リストの組み立て

set legs {}
if {$opt(all)} {
    puts "== --all: フル matrix ([llength $ALL_VERSIONS] versions x 2 modes) =="
    puts "   イメージだけで概算 12〜16GB のディスクを消費します"
    puts "   (1 version ≈ 0.9〜1.1GB + threaded 差分、掃除は --clean と podman rmi)"
    if {!$opt(force-yes) && ![confirm "続行しますか?"]} { exit 1 }
    foreach v $ALL_VERSIONS { lappend legs $v-non-threaded $v-threaded }
} else {
    if {![llength $specs]} usage
    foreach spec $specs {
        if {[regexp {^\d+\.\d+-threaded$} $spec]} {
            lappend legs $spec
        } elseif {[regexp {^\d+\.\d+-non-threaded$} $spec]} {
            lappend legs $spec
        } elseif {[regexp {^\d+\.\d+$} $spec]} {
            if {$opt(both)} {
                lappend legs $spec-non-threaded $spec-threaded
            } else {
                # 裸の VERSION は docker タグの既定に合わせ非 threaded
                lappend legs $spec-non-threaded
            }
        } else {
            puts stderr "cannot parse version spec: $spec (expected X.Y\[-threaded|-non-threaded])"
            exit 2
        }
    }
}

#========================================
# イメージタグ解決 (fallback 連鎖)

proc imageExists {image} {
    global engine
    expr {![catch {exec {*}$engine image exists $image 2>/dev/null}]}
}

proc resolveImage {ver mode} {
    global opt IMAGE_PREFIX
    set suffix [expr {$mode eq "threaded" ? "-threaded" : ""}]
    if {$opt(image-tag) ne ""} {
        set candidates [list $opt(image-tag)]
    } elseif {[regexp {^5\.(\d+)$} $ver -> minor] && $minor <= 26} {
        # 5.20〜5.26 の素タグは stretch ベースで、glibc ヘッダが古すぎて
        # macrogen の preprocess が通らない (__intN_t 未展開等)。buster を優先
        set candidates [list $ver$suffix-buster $ver$suffix]
    } else {
        set candidates [list $ver$suffix $ver$suffix-buster]
    }
    foreach tag $candidates {
        set image $IMAGE_PREFIX:$tag
        if {!$opt(pull) && [imageExists $image]} { return $image }
        if {[runCmd {*}$::engine pull $image] == 0} { return $image }
    }
    puts stderr "FATAL: no usable image for $ver ($mode); tried: $candidates"
    puts stderr "       (tag 一覧: https://hub.docker.com/_/perl — --image-tag で明示指定可)"
    return ""
}

#========================================
# downstream の準備 (呼び出しあたり 1 回): ソース取得 → scratch copy →
# [patch.crates-io] 注入。ユーザーの checkout は絶対に汚さない。
# 注: scratch の Cargo.lock は crates.io 版 macrogen を pin したままなので、
# 実際の patch 適用は inner.sh の `cargo update -p libperl-macrogen` が行う

set downstreamScratch [file join $stateDir downstream-src]

proc prepareDownstream {} {
    global opt stateDir downstreamScratch DOWNSTREAM_URL

    if {$opt(libperl-rs) ne ""} {
        set src [file normalize $opt(libperl-rs)]
        if {![file isdirectory $src]} {
            puts stderr "FATAL: --libperl-rs $src is not a directory"
            return 0
        }
        puts "== downstream source: local checkout $src (uncommitted changes included) =="
    } else {
        set src [file join $stateDir libperl-rs]
        if {[file isdirectory [file join $src .git]]} {
            if {[runCmd git -C $src fetch --depth 1 origin $opt(branch)] != 0} { return 0 }
            if {[runCmd git -C $src reset --hard FETCH_HEAD] != 0} { return 0 }
        } else {
            if {[runCmd git clone --depth 1 --branch $opt(branch) \
                     $DOWNSTREAM_URL $src] != 0} { return 0 }
        }
    }

    runCmd rm -rf $downstreamScratch
    runCmd mkdir -p $downstreamScratch
    if {$opt(dryrun)} {
        puts "# tar -C $src (exclude .git, target) | tar -C $downstreamScratch -x"
        puts "# append \[patch.crates-io] libperl-macrogen = { path = \"/app\" } to Cargo.toml"
        return 1
    }
    exec tar -C $src --exclude=./.git --exclude=./target --exclude=./*/target \
        -cf - . | tar -C $downstreamScratch -xf -

    set fh [open [file join $downstreamScratch Cargo.toml] a]
    puts $fh ""
    puts $fh "# --- appended by libperl-macrogen scripts/multi-perl.tcl (scratch copy only) ---"
    puts $fh {[patch.crates-io]}
    puts $fh {libperl-macrogen = { path = "/app" }}
    close $fh
    puts "== downstream scratch ready: $downstreamScratch (branch/source: $opt(branch)) =="
    return 1
}

#========================================
# leg 実行

proc commonMounts {outLeg} {
    global repoDir stateDir SELINUX
    list \
        -v $repoDir:/app$SELINUX \
        -v [file join $stateDir rust-home]:/opt/rust$SELINUX \
        -v [file join $stateDir target]:/cargo-target-root$SELINUX \
        -v $outLeg:/out$SELINUX
}

proc runLegSmoke {leg image} {
    global opt engine stateDir outRoot passthru
    set outLeg [file join $outRoot $leg]
    runCmd mkdir -p [file join $stateDir rust-home] [file join $stateDir target] $outLeg

    set subcmd [expr {$opt(shell) ? "shell" : "smoke"}]
    set cmd [list {*}$engine run --rm]
    if {[haveTty]} { lappend cmd -it }
    lappend cmd {*}[commonMounts $outLeg] \
        -e MP_LEG=$leg \
        -e MP_SHELL_ON_FAIL=$opt(shell-on-fail) \
        -e MP_BASELINE=$opt(baseline) \
        -e MP_STRICT=$opt(strict) \
        $image bash /app/scripts/multi-perl-inner.sh $subcmd
    if {[llength $passthru]} { lappend cmd {*}$passthru }
    runCmd {*}$cmd
}

proc runLegDownstream {leg image} {
    global opt engine stateDir outRoot downstreamScratch SELINUX
    set outLeg [file join $outRoot $leg]
    set dsTarget [file join $stateDir target-downstream $leg]
    runCmd mkdir -p $dsTarget $outLeg

    set cmd [list {*}$engine run --rm]
    if {[haveTty]} { lappend cmd -it }
    lappend cmd {*}[commonMounts $outLeg] \
        -v $downstreamScratch:/downstream$SELINUX \
        -v $dsTarget:/downstream-target$SELINUX \
        -e MP_LEG=$leg \
        -e MP_SHELL_ON_FAIL=$opt(shell-on-fail) \
        -e MP_USE_ARCHIVE=$opt(use-archive) \
        -e MP_DOWNSTREAM_TEST=$opt(downstream-test) \
        $image bash /app/scripts/multi-perl-inner.sh downstream
    runCmd {*}$cmd
}

#========================================
if {$opt(downstream) && ![prepareDownstream]} { exit 1 }

set legStatus [dict create]
set anyFailed 0
foreach leg $legs {
    puts "===== leg: $leg ====="
    if {[regexp {^(.+)-non-threaded$} $leg -> ver]} {
        set mode non-threaded
    } else {
        regexp {^(.+)-threaded$} $leg -> ver
        set mode threaded
    }

    set image [resolveImage $ver $mode]
    if {$image eq ""} {
        dict set legStatus $leg "NG (no image)"
        set anyFailed 1
        continue
    }

    if {[runLegSmoke $leg $image] == 0} {
        set smokeStatus OK
    } else {
        set smokeStatus NG
        set anyFailed 1
    }

    if {$opt(downstream)} {
        puts "----- leg: $leg (downstream) -----"
        if {[runLegDownstream $leg $image] == 0} {
            set dsStatus OK
        } else {
            set dsStatus NG
            set anyFailed 1
        }
        dict set legStatus $leg "smoke=$smokeStatus downstream=$dsStatus"
    } else {
        dict set legStatus $leg $smokeStatus
    }
}

puts ""
puts "===== multi-perl summary ====="
foreach leg $legs {
    puts [format "  %-22s %s" $leg [dict get $legStatus $leg]]
}
if {$anyFailed} {
    puts "results under: $outRoot/<leg>/"
}
exit $anyFailed
