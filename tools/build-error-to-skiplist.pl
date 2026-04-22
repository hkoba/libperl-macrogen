#!/usr/bin/env perl
# build-error-to-skiplist.pl
#
# cargo build --message-format のエラーログ（`--> …/macro_bindings.rs:LINE:COL`
# 形式を含む）と macro_bindings.rs を入力に取り、
# エラー発生行を含む関数の `pub unsafe fn NAME` を拾って
# 1 行 1 名の skip-list を stdout に出す。
#
# 使い方:
#   ./build-error-to-skiplist.pl tmp/build-error.log tmp/macro_bindings.rs > tmp/skip.txt

use strict;
use warnings;

@ARGV == 2 or die "usage: $0 <build-error.log> <macro_bindings.rs>\n";
my ($log_path, $bindings_path) = @ARGV;

# 1) エラー行番号を収集
open my $lf, '<', $log_path or die "open $log_path: $!";
my %err_lines;
while (<$lf>) {
    # --> /path/to/macro_bindings.rs:LINE:COL
    if (m{--> \S*/macro_bindings\.rs:(\d+):\d+}) {
        $err_lines{$1} = 1;
    }
}
close $lf;

# 2) macro_bindings.rs を走査し、各行で直近の `pub unsafe fn NAME` を覚えておく
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

# 3) ソート出力
print "# Generated from $log_path\n";
print "# Functions whose bodies produced cargo build errors.\n";
for my $name (sort keys %skip) {
    print "$name\n";
}
