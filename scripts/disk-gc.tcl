#!/usr/bin/tclsh
# disk-gc.tcl — target/ と tmp/ の軽量化 (report / clean)
#
# usage: scripts/disk-gc.tcl                  # report (読み取り専用)
#        scripts/disk-gc.tcl -assert-free 10  # 空き < 10G なら report を出して exit 1
#        scripts/disk-gc.tcl -clean [-n]      # 既定 tier の削除 (confirm あり)
#        scripts/disk-gc.tcl -clean -deep     # + ビルドキャッシュ全部
#
# 方針: 厳密さより速報性と致命的事態の回避を優先する。
# - 検査は df (即答) + 既知ディレクトリの du -s のみ (find 等の深い走査はしない)
# - 削除はホワイトリスト方式。対象は repo 内の tmp/ と target/ に限定し、
#   実行直前に assertDeletable で配下チェックする
#
# 既定 tier (-clean):
# - 旧版 apidoc-cache (tmp/multi-perl/out/*/apidoc-cache/.../apidoc-v* のうち
#   src/apidoc_data.rs の APIDOC_DATA_VERSION と不一致のもの)
# - keep ポリシー対象外の tmp/multi-perl/target-downstream/<leg>
#   (既定 keep: publish.tcl の下流検証レッグ + mtime が -older-than 日以内)
# - tmp/multi-perl/downstream-src (毎 run 再作成される scratch)
#
# -deep 追加分:
# - target-downstream の全レッグ (keep ポリシー無視)
# - tmp/multi-perl/target (Debian codename 別 macrogen ビルドキャッシュ)
# - tmp/multi-perl/rust-home (rustup/cargo home、再ダウンロードで復元)
# - ホスト target/ (cargo clean で)
#
# 絶対に触らないもの:
# - tmp/perls/ (golden perl、再構築が高価 — CLAUDE.md 参照)
# - tmp/multi-perl/out/<leg>/ の成果物・ログ (bindings.rs / macro_bindings.rs /
#   *.log / summary.json — tools/build-error-to-vpatches.pl の入力)
# - 現行版 apidoc-cache (APIDOC_DATA_VERSION 一致分)
# - tmp/multi-perl/libperl-rs (shallow clone キャッシュ、微小)

#----------------------------------------
# my librun (publish.tcl と同じ)
package require cmdline

proc RUN args {
    puts "# $args"
    if {$::opts(n)} return
    if {[lindex $args end-1] ni {">" ">@" ">>"}} {
        lappend args >@ stdout
    }
    =RUN {*}$args
}

proc =RUN args {
    exec -ignorestderr {*}$args 2>@ stderr
}

proc haveTty {} {
    # -mode オプションは端末 (serial 系 channel) にしか存在しない
    expr {![catch {chan configure stdin -mode}]}
}

proc confirm {msg} {
    if {$::opts(yes) || $::opts(n)} { return 1 }
    puts -nonewline "$msg \[y/N] "
    flush stdout
    if {[gets stdin ans] < 0} { return 0 }
    string match -nocase y* [string trim $ans]
}
#----------------------------------------

proc readApidocDataVersion {content} {
    if {![regexp {pub const APIDOC_DATA_VERSION: &str = "([^"]+)"} $content -> ver]} {
        error "APIDOC_DATA_VERSION not found"
    }
    return $ver
}

# repo の FS の空き容量 (GiB)。df 1 発なので即答
proc freeGiB {} {
    set kb [lindex [=RUN df --output=avail -k . | tail -1] 0]
    expr {$kb / 1048576.0}
}

# du -sm 一括 1 発。存在しない path は除外。{mb path} のリストを返す
proc duMB {paths} {
    set existing {}
    foreach p $paths { if {[file exists $p]} { lappend existing $p } }
    if {![llength $existing]} { return {} }
    set result {}
    foreach line [split [=RUN du -sm {*}$existing] \n] {
        if {[regexp {^(\d+)\s+(.*)$} $line -> mb path]} {
            lappend result [list $mb $path]
        }
    }
    return $result
}

proc fmtG {mb} { format %.1fG [expr {$mb / 1024.0}] }

proc sumMB {duList} {
    set total 0
    foreach e $duList { incr total [lindex $e 0] }
    return $total
}

# 削除して良い path か (ホワイトリスト方式の最終防波堤)。
# cwd = repoDir 前提。symlink 事故・変数壊れで repo 外や保護対象を
# 消してしまう事態をここで止める
proc assertDeletable {path} {
    set norm [file normalize $path]
    set tmpRoot [file normalize tmp]
    set tgtRoot [file normalize target]
    if {!([string match $tmpRoot/* $norm]
          || $norm eq $tgtRoot || [string match $tgtRoot/* $norm])} {
        error "削除対象が repo の tmp/・target/ 配下ではありません: $norm"
    }
    foreach protected [list [file normalize tmp/perls] \
                           [file normalize tmp/multi-perl/libperl-rs]] {
        if {$norm eq $protected || [string match $protected/* $norm]} {
            error "保護対象です: $norm"
        }
    }
    # out/ 配下で消して良いのは apidoc-cache のみ (成果物・ログは保持)
    set outRoot [file normalize tmp/multi-perl/out]
    if {[string match $outRoot/* $norm] && ![string match */apidoc-cache/* $norm]} {
        error "out/ 配下は apidoc-cache 以外削除しません: $norm"
    }
}

#----------------------------------------

array set ::opts [cmdline::getoptions ::argv {
    {n "dry-run (削除コマンドと回収サイズの表示のみ)"}
    {clean "既定 tier の削除を実行"}
    {deep "-clean に加えビルドキャッシュ全部 (要 -clean)"}
    {yes "確認プロンプトを省略"}
    {assert-free.arg "" "空き容量 (GiB) がこの値未満なら report を出して exit 1"}
    {keep-legs.arg "5.30-threaded 5.44-threaded" "-clean 時に保持する target-downstream レッグ"}
    {older-than.arg 7 "mtime がこの日数以内の target-downstream レッグは保持"}
}]

if {$::opts(deep) && !$::opts(clean)} {
    error "-deep は -clean と併用して下さい (単体では何もしません)"
}

cd [file dirname [file dirname [file normalize [info script]]]]
set stateDir tmp/multi-perl

#----------------------------------------
# 空き容量チェック (df のみ・即答)

set free [freeGiB]
puts [format "# %s: 空き %.1fG" [pwd] $free]

set assertFailed 0
if {$::opts(assert-free) ne ""} {
    if {$free >= $::opts(assert-free)} {
        puts "# disk-gc: OK (>= $::opts(assert-free)G)"
        exit 0
    }
    puts "# disk-gc: 空きが $::opts(assert-free)G 未満です。以下の report を確認し\
 scripts/disk-gc.tcl -clean を検討して下さい"
    set assertFailed 1
}

#----------------------------------------
# 削除候補の分類 (glob + file mtime のみ。深い走査はしない)

# 旧版 apidoc-cache — APIDOC_DATA_VERSION が読めない時は全部保持して警告
set staleApidoc {}
set apidocVer ?
if {[catch {
    set apidocVer [readApidocDataVersion [=RUN cat src/apidoc_data.rs]]
    foreach d [glob -nocomplain -types d \
                   $stateDir/out/*/apidoc-cache/libperl-macrogen/apidoc-v*] {
        if {[file tail $d] ne "apidoc-v$apidocVer"} { lappend staleApidoc $d }
    }
} err]} {
    puts "# WARN: apidoc-cache の stale 判定をスキップ ($err)"
}

# target-downstream レッグの keep/drop 判定
set now [clock seconds]
set legsDrop {}
set legsKeep {}
foreach d [lsort [glob -nocomplain -types d $stateDir/target-downstream/*]] {
    set leg [file tail $d]
    set ageDays [expr {($now - [file mtime $d]) / 86400}]
    if {$leg in $::opts(keep-legs)} {
        lappend legsKeep [list $d keep-leg]
    } elseif {$ageDays < $::opts(older-than)} {
        lappend legsKeep [list $d "${ageDays}日前"]
    } else {
        lappend legsDrop [list $d "${ageDays}日前"]
    }
}

#----------------------------------------
# du (既知ディレクトリの一括 1 発 + 削除候補の apidoc-cache 分)

set duTable [duMB [concat \
                       [list target tmp/perls] \
                       [lmap d {out target rust-home downstream-src libperl-rs} \
                            {file join $stateDir $d}] \
                       [glob -nocomplain -types d $stateDir/target-downstream/*]]]
array set duOf {}
foreach e $duTable { set duOf([lindex $e 1]) [lindex $e 0] }
proc mbOf {path} { expr {[info exists ::duOf($path)] ? $::duOf($path) : 0} }

set staleDu [duMB $staleApidoc]
set staleMB [sumMB $staleDu]

puts "== 使用量 =="
foreach e [lsort -integer -decreasing -index 0 $duTable] {
    lassign $e mb path
    puts [format "  %7s  %s" [fmtG $mb] $path]
}

#----------------------------------------
# 既定 tier の候補と回収見込み

set dropMB $staleMB
puts "== 削除候補 (既定 tier / -clean) =="
if {[llength $staleApidoc]} {
    puts [format "  %7s  旧版 apidoc-cache %d dirs (現行 apidoc-v%s は保持)" \
              [fmtG $staleMB] [llength $staleApidoc] $apidocVer]
}
foreach e $legsDrop {
    lassign $e d age
    incr dropMB [mbOf $d]
    puts [format "  %7s  %s (%s)" [fmtG [mbOf $d]] $d $age]
}
if {[file exists $stateDir/downstream-src]} {
    incr dropMB [mbOf $stateDir/downstream-src]
    puts [format "  %7s  %s (毎 run 再作成)" \
              [fmtG [mbOf $stateDir/downstream-src]] $stateDir/downstream-src]
}
puts [format "  合計 %s" [fmtG $dropMB]]
if {[llength $legsKeep]} {
    puts "  保持: [join [lmap e $legsKeep {
        format {%s (%s)} [file tail [lindex $e 0]] [lindex $e 1]
    }] {, }]"
}

# -deep 追加分
set deepMB 0
foreach e $legsKeep { incr deepMB [mbOf [lindex $e 0]] }
foreach d [list $stateDir/target $stateDir/rust-home target] {
    incr deepMB [mbOf $d]
}
puts "== -deep 追加分 (target-downstream 全レッグ + $stateDir/target +\
 rust-home + target/) =="
puts [format "  合計 %s" [fmtG $deepMB]]

if {$assertFailed} { exit 1 }
if {!$::opts(clean)} { exit 0 }

#----------------------------------------
# 削除の実行

set targets [concat $staleApidoc [lmap e $legsDrop {lindex $e 0}]]
if {[file exists $stateDir/downstream-src]} {
    lappend targets $stateDir/downstream-src
}
set totalMB $dropMB
if {$::opts(deep)} {
    foreach e $legsKeep { lappend targets [lindex $e 0] }
    foreach d [list $stateDir/target $stateDir/rust-home] {
        if {[file exists $d]} { lappend targets $d }
    }
    incr totalMB $deepMB
}

if {![llength $targets] && !$::opts(deep)} {
    puts "# 削除対象なし"
    exit 0
}

if {!$::opts(yes) && !$::opts(n) && ![haveTty]} {
    error "非対話実行です。削除には -yes を付けて下さい (何も削除していません)"
}
if {![confirm "上記 [llength $targets] 項目 (約 [fmtG $totalMB]) を削除します。よろしいですか?"]} {
    error "中止しました (何も削除していません)"
}

foreach t $targets { assertDeletable $t }
if {[llength $targets]} {
    RUN rm -rf {*}$targets
}
if {$::opts(deep)} {
    # ホスト target/ は cargo に任せる (target/ 直下の一部ファイルは残る)
    RUN cargo clean
}

if {!$::opts(n)} {
    puts [format "# 削除後: 空き %.1fG" [freeGiB]]
}
