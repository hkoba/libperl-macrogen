#!/usr/bin/tclsh
# publish.tcl — libperl-macrogen のリリース手順 (libperl-rs/publish.tcl の移植)
#
# usage: ./publish.tcl [-n] [-step {1 2 ...}] [-smoke LEGS] [-downstream LEGS] [-yes]
#
# 本 crate 固有の注意点 (doc/plan/todo-2026-08-19.md ほか) をステップ化している:
#
# 1. apidoc.tar.gz の再同梱 (STEP 2): build.rs は「ローカル apidoc/ →
#    同梱 apidoc.tar.gz → 空 placeholder」の優先順で解決する。crates.io
#    利用者は apidoc/ を持たない (Cargo.toml の exclude 対象) ため、
#    publish 前に apidoc/ から tar.gz を再生成して commit する
# 2. APIDOC_DATA_VERSION の整合 (STEP 1): 前回リリース以降に apidoc/ が
#    変わっているのに src/apidoc_data.rs の定数が同じなら、利用者側の
#    展開キャッシュが stale のまま使われるため fail する
# 3. publish 前の下流検証 (STEP 5): scripts/multi-perl.tcl --downstream で
#    libperl-rs (libperl-sys) を working tree の macrogen でビルドする。
#    CI 側の同等物は downstream.yml (workflow_dispatch)
# 4. ローカル検証 (STEP 3, 4): cargo test (golden は 5.42-threaded 基準、
#    他版では自動 skip) + 代表 leg のスモーク

#----------------------------------------
# my librun
package require cmdline

proc RUN args {
    puts "# $args"
    if {$::opts(n)} return
    # リダイレクトが指定されていない時は stdout へリダイレクト。末尾のみ認識。
    if {[lindex $args end-1] ni {">" ">@" ">>"}} {
        lappend args >@ stdout
    }
    =RUN {*}$args
}

proc =RUN args {
    exec -ignorestderr {*}$args 2>@ stderr
}

proc o_dryrun {} {
    if {$::opts(n)} {list -n}
}

# Tcl 自体のコマンドを dry-run にしたいときは ** を使う
proc ** args {
    puts "# $args"
    if {$::opts(n)} return
    {*}$args
}
#----------------------------------------

proc readPackageVersion {tomlFn} {
    =RUN perl -nle {
        next unless m{^\[package\]} ... m{^\[};
        /^version = "([^\"]+)"/ and print $1 and ++$ok;
        END {exit 1 if not $ok}
    } $tomlFn
}

proc incrementMinorVersion verStr {
    set verList [split $verStr .]
    set minor [lindex $verList end]
    set newVerList [lreplace $verList end end [incr minor]]
    join $newVerList .
}

proc readApidocDataVersion {content} {
    if {![regexp {pub const APIDOC_DATA_VERSION: &str = "([^"]+)"} $content -> ver]} {
        error "APIDOC_DATA_VERSION not found"
    }
    return $ver
}

proc confirm {msg} {
    if {$::opts(yes) || $::opts(n)} { return 1 }
    puts -nonewline "$msg \[y/N] "
    flush stdout
    if {[gets stdin ans] < 0} { return 0 }
    string match -nocase y* [string trim $ans]
}

#----------------------------------------

array set ::opts [cmdline::getoptions ::argv {
    {n "dry-run"}
    {q "quiet"}
    {step.arg "" "Run only specific steps"}
    {smoke.arg "5.42" "smoke 検証の対象バージョン (multi-perl.tcl --both に渡す)"}
    {downstream.arg "5.30-threaded 5.44-threaded" "下流検証の legs"}
    {yes "publish 前の確認プロンプトを省略"}
}]

proc STEP {n message command} {
    if {$::opts(step) eq ""} {
        puts "# ($n) $message"
    } elseif {$n ni $::opts(step)} {
        return;
    }
    uplevel #0 $command
}

#----------------------------------------

cd [file dirname [file normalize [info script]]]

set ::currentVersion [readPackageVersion Cargo.toml]
set ::newVersion     [incrementMinorVersion $::currentVersion]
set ::lastTag        [=RUN git tag -l v* --sort=-v:refname | head -1]

puts "# libperl-macrogen $::currentVersion -> $::newVersion (前回 tag: $::lastTag)"

STEP 1 "前提チェック (clean tree / main / APIDOC_DATA_VERSION 整合)" {
    if {[=RUN git status --porcelain --untracked-files=no] ne ""} {
        error "working tree が clean ではありません (commit してから実行して下さい)"
    }
    if {[=RUN git branch --show-current] ne "main"} {
        error "main ブランチではありません"
    }

    RUN git pull

    # 前回リリース以降に apidoc/ が変わったなら APIDOC_DATA_VERSION も
    # 変わっていなければならない (利用者側キャッシュの stale 化防止)
    set apidocChanged [catch {=RUN git diff --quiet $::lastTag..HEAD -- apidoc}]
    set verNow  [readApidocDataVersion [=RUN cat src/apidoc_data.rs]]
    set verLast [readApidocDataVersion [=RUN git show $::lastTag:src/apidoc_data.rs]]
    if {$apidocChanged && $verNow eq $verLast} {
        error "apidoc/ が $::lastTag から変更されているのに APIDOC_DATA_VERSION が\
 \"$verNow\" のままです。src/apidoc_data.rs を bump して下さい"
    }
    puts "# APIDOC_DATA_VERSION: $verLast ($::lastTag) -> $verNow (apidoc/ changed: $apidocChanged)"
}

STEP 2 "apidoc.tar.gz を apidoc/ から再生成 (crates.io 同梱用)" {
    # 決定的 tar (mtime/owner を固定) — 内容が同じなら再生成しても diff が出ない
    RUN tar --sort=name --owner=0 --group=0 --numeric-owner \
        --mtime=2020-01-01T00:00:00Z -cf - apidoc | gzip -n > apidoc.tar.gz
    if {[=RUN git status --porcelain apidoc.tar.gz] ne ""} {
        RUN git commit -m "apidoc.tar.gz を apidoc/ から再生成 (release 同梱)" apidoc.tar.gz
    } else {
        puts "# apidoc.tar.gz は最新 (commit 不要)"
    }
}

STEP 3 "ローカル検証 (cargo test — golden は 5.42-threaded 基準)" {
    set ambient [=RUN perl -MConfig -le {print "$Config{version} $Config{usethreads}"}]
    if {![string match "5.42.* define" $ambient]} {
        puts "# WARN: ambient perl は '$ambient' — golden 回帰テストは自動 skip されます"
        puts "#       (基準 perl での実走は tmp/perls/v5.42.3 を PATH に載せるか CI の golden job で)"
    }
    RUN cargo test
}

STEP 4 "代表 leg のスモーク (podman)" {
    RUN scripts/multi-perl.tcl --both {*}$::opts(smoke)
}

STEP 5 "publish 前の下流検証 (libperl-sys ビルド、podman)" {
    # working tree の macrogen を [patch.crates-io] で注入してビルドする。
    # CI 側で行う場合は downstream.yml を workflow_dispatch で起動する
    RUN scripts/multi-perl.tcl --downstream {*}$::opts(downstream)
}

STEP 6 "バージョン bump + build 検証 + commit" {
    RUN perl -i -s -ple {
        if (m{^\[package\]} ... m{^\[}) {
            s/^version = "(?:[^\"]+)"/version = "$newVersion"/
            and print STDERR "# Updated $ARGV: $_" and ++$ok;
        }
        END {exit 1 if not $ok}
    } -- -newVersion=$::newVersion Cargo.toml

    RUN cargo build
    RUN git commit -m "Bump libperl-macrogen to $::newVersion" Cargo.toml Cargo.lock
}

STEP 7 "パッケージ内容の検査 (apidoc.tar.gz 同梱 / apidoc/ 除外)" {
    # dry-run では STEP 6 の commit が行われないため dirty を許容する
    set extra [expr {$::opts(n) ? {--allow-dirty} : {}}]
    set files [split [=RUN cargo package --list {*}$extra] \n]
    if {"apidoc.tar.gz" ni $files} {
        error "cargo package に apidoc.tar.gz が含まれていません"
    }
    foreach f $files {
        if {[string match apidoc/* $f]} {
            error "cargo package に apidoc/ 配下が含まれています: $f (exclude 設定を確認)"
        }
    }
    puts "# package OK: apidoc.tar.gz 同梱 / apidoc/ 除外 ([llength $files] files)"
}

STEP 8 publish {
    if {![confirm "crates.io へ libperl-macrogen $::newVersion を publish します。よろしいですか?"]} {
        error "中止しました"
    }
    RUN cargo publish
}

STEP 9 "事後処理 (tag + push)" {
    RUN git tag v$::newVersion -m "Bump version to v$::newVersion"

    # 注: Tcl の exec は `&&` をシェル演算子として解釈しない (`&&` が
    # git の引数になってしまう) ため、原本と違い 2 コマンドに分けている
    RUN git push --tags
    RUN git push

    puts "# 次の手作業:"
    puts "#   - libperl-rs 側の依存を \"$::newVersion\" に更新して CI 確認"
    puts "#   - libperl-rs#16: CI matrix の '5' (= 5.44) セルを復帰"
    puts "#   - 必要なら downstream.yml を dispatch して公開版でも検証"
}
