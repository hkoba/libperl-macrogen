#!/usr/bin/env perl
# build-error-to-vpatches.pl
#
# 既存の build-error-to-skiplist.pl を拡張し、apidoc/v$X.$Y.patches.json
# 形式（skip_codegen エントリだけの最小 patch ファイル）を出力する。
# CI で版別に失敗する関数を「その版でだけ codegen 抑制」するための version-specific
# patches を機械的に生成するために使う。
#
# 使い方:
#   tools/build-error-to-vpatches.pl <perl-major.minor> <build-error.log> <macro_bindings.rs>
#     > apidoc/v5.36.patches.json
#
# 入力:
#   - <perl-major.minor>: 例 "5.36"。出力 JSON の comment / reason に埋め込む
#   - <build-error.log>: cargo build のエラーログ（`--> .../macro_bindings.rs:LINE:COL` を含む）
#   - <macro_bindings.rs>: 該当 perl 版の生成物（CI artifact から取得）
#
# 既存の `apidoc/v$X.$Y.patches.json` を更新する場合は、本ツールで再生成して
# diff を確認しつつ上書きすること。

use strict;
use warnings;

@ARGV == 3 or die "usage: $0 <perl-major.minor> <build-error.log> <macro_bindings.rs>\n";
my ($version, $log_path, $bindings_path) = @ARGV;

$version =~ /^\d+\.\d+$/
    or die "invalid version: '$version' (expected major.minor like 5.36)\n";

# 1) エラー行番号を収集
#    `note: function defined here` 等の note ブロックが指す span は「呼ばれた側」
#    であってエラー箇所ではないので除外する (例: 呼び出し側の引数型不一致で
#    正常な callee が failing 扱いになる誤検出があった — 2026-08-29 の 5.26 採取で
#    sv_cmp_flags が混入)。error/warning ヘッダ直後の primary span のみ拾う。
open my $lf, '<', $log_path or die "open $log_path: $!";
my %err_lines;
my $in_primary = 0;
while (<$lf>) {
    if (/^error(?:\[E\d+\])?: /) {
        $in_primary = 1;
    } elsif (/^(?:warning|note|help)(?:\[.*?\])?: /) {
        $in_primary = 0;
    }
    if ($in_primary && m{--> \S*/macro_bindings\.rs:(\d+):\d+}) {
        $err_lines{$1} = 1;
    }
}
close $lf;

# 2) macro_bindings.rs を走査し、各エラー行を含む関数を特定
open my $bf, '<', $bindings_path or die "open $bindings_path: $!";
my $current_fn;
my %skip;
while (my $line = <$bf>) {
    if ($line =~ /^\s*pub(?:\s+unsafe)?\s+fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[(<]/) {
        $current_fn = $1;
    }
    if (exists $err_lines{$.}) {
        if (defined $current_fn) {
            $skip{$current_fn} = 1;
        }
    }
}
close $bf;

# 3) JSON 出力（apidoc patches schema_version: 1 互換）
my @names = sort keys %skip;
print "{\n";
print "  \"schema_version\": 1,\n";
print "  \"comment\": \"Auto-generated skip_codegen patches for perl ${version}. ";
print "Functions whose generated body fails to type-check on this perl version ";
print "(often due to macro expansion differences vs newer perl). ";
print "Re-run tools/build-error-to-vpatches.pl after each perl-headers update or ";
print "libperl-macrogen change.\",\n";
print "  \"patches\": [\n";
for my $i (0 .. $#names) {
    my $n = $names[$i];
    my $comma = $i == $#names ? "" : ",";
    print "    { \"name\": \"$n\", \"kind\": \"skip_codegen\", ";
    print "\"reason\": \"type-check fails on perl ${version} (auto-generated)\" }${comma}\n";
}
print "  ]\n";
print "}\n";

# stderr に件数を出す
my $count = scalar @names;
print STDERR "[build-error-to-vpatches] perl $version: $count function(s) marked skip_codegen\n";
