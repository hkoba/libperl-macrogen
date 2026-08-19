#!/usr/bin/env perl
# multi-perl-smoke.pl — ambient perl に対する macrogen のスモーク検査 harness
#
# PATH 上の perl (= $^X) を対象に macrogen を実行し、生成物を
# scripts/multi-perl-expect/ の期待値と突き合わせる。podman コンテナ内・
# GitHub CI (actions-setup-perl)・ホスト (tmp/perls/ を PATH 前置) の
# どこでも同じように動くことを狙い、core モジュールのみで書く
# (perl 5.20 で動作すること)。
#
# 使い方:
#   perl scripts/multi-perl-smoke.pl --require 5.42-threaded \
#       [--out DIR] [--expect DIR] [--macrogen PATH] [--baseline] [--strict] \
#       [-- <macrogen への追加引数>...]
#
# 検査結果: PASS / FAIL / XFAIL (known_failures 登録済みの失敗) /
# XPASS (登録済みなのに成功 = 期待値を引き締めよ) / WARN / INFO。
# exit 0 = FAIL なし (--strict では XFAIL/XPASS も FAIL 扱い)。
# --baseline は全検査を実行して known_failures JSON 断片を stdout に出力し、
# 常に exit 0 (期待値ファイルの seed 用)。

use strict;
use warnings;
use Config;
use Getopt::Long;
use File::Path qw(make_path);
use File::Basename qw(dirname);
use File::Spec;
use Cwd qw(abs_path);
use JSON::PP;
use POSIX qw(ceil);

my $repo_root = abs_path(File::Spec->catdir(dirname(abs_path($0)), File::Spec->updir));

my %opt = (expect => File::Spec->catdir($repo_root, 'scripts', 'multi-perl-expect'));
GetOptions(\%opt, 'require=s', 'out=s', 'expect=s', 'macrogen=s',
           'baseline!', 'strict!', 'help!')
    or die "usage error (try --help)\n";
if ($opt{help}) {
    print <<"END";
usage: perl $0 --require VERSION[-MODE] [options]
  --require 5.42-threaded   ambient perl がこの版・モードであることを検査
                            (MODE: threaded | non-threaded; 省略時はモード不問)
  --out DIR       生成物・ログ・summary.json の出力先
                  (既定: tmp/multi-perl/out/<ver>-<mode>)
  --expect DIR    期待値ディレクトリ (既定: scripts/multi-perl-expect)
  --macrogen PATH macrogen バイナリ
                  (既定: \$CARGO_TARGET_DIR//target /debug/libperl-macrogen)
  --baseline      known_failures JSON 断片を出力して exit 0 (期待値の seed 用)
  --strict        XFAIL/XPASS も FAIL 扱い
  -- ARGS...      残りは macrogen にそのまま渡す (例: -- -I /extra/include)
END
    exit 0;
}
my @macrogen_extra_args = @ARGV;

chdir $repo_root or die "chdir $repo_root: $!";

# ── ambient perl の検出 ──────────────────────────────────────
my ($ver_major, $ver_minor) = $Config{version} =~ /^(\d+)\.(\d+)/
    or die "cannot parse perl version: $Config{version}\n";
my $threaded = (($Config{usethreads} || '') eq 'define') ? 1 : 0;
my $mode_name = $threaded ? 'threaded' : 'non-threaded';
my $leg = "$ver_major.$ver_minor-$mode_name";

my $out_dir = $opt{out}
    // File::Spec->catdir($repo_root, 'tmp', 'multi-perl', 'out', $leg);
make_path($out_dir);

my $macrogen = $opt{macrogen} // do {
    my $target = $ENV{CARGO_TARGET_DIR} // File::Spec->catdir($repo_root, 'target');
    $target = File::Spec->rel2abs($target, $repo_root);
    File::Spec->catfile($target, 'debug', 'libperl-macrogen');
};

# ── 検査結果の記録 ──────────────────────────────────────────
my @results;    # { id, status, detail }
sub record {
    my ($id, $status, $detail) = @_;
    push @results, { id => $id, status => $status,
                     (defined $detail ? (detail => $detail) : ()) };
    printf "%-6s %s%s\n", $status, $id, (defined $detail ? " - $detail" : "");
}

# known_failures の適用: 失敗を XFAIL に降格、成功に XPASS 警告
my %xfail_assert;       # assertion id → 1
my %xfail_must_gen;     # 関数名 → 1
sub record_checked {
    my ($id, $ok, $fail_detail, $pass_detail) = @_;
    my $known = $xfail_assert{$id} ? 1 : 0;
    if ($ok) {
        record($id, $known ? 'XPASS' : 'PASS', $pass_detail);
    } else {
        record($id, $known ? 'XFAIL' : 'FAIL', $fail_detail);
    }
    return $ok;
}

# ── サブプロセス実行 (リダイレクト付き、shell 非経由) ────────
sub run_cmd {
    my ($cmd, %redirect) = @_;
    my $pid = fork;
    die "fork: $!" unless defined $pid;
    if (!$pid) {
        if ($redirect{stdin}) {
            open STDIN, '<', $redirect{stdin} or die "open $redirect{stdin}: $!";
        }
        if ($redirect{stdout}) {
            open STDOUT, '>', $redirect{stdout} or die "open $redirect{stdout}: $!";
        }
        if ($redirect{stderr}) {
            open STDERR, '>', $redirect{stderr} or die "open $redirect{stderr}: $!";
        }
        exec @$cmd;
        die "exec $$cmd[0]: $!";
    }
    waitpid($pid, 0);
    return $?;
}

sub which {
    my ($name) = @_;
    for my $dir (File::Spec->path) {
        my $f = File::Spec->catfile($dir, $name);
        return $f if -x $f;
    }
    return undef;
}

sub slurp {
    my ($path) = @_;
    open my $fh, '<', $path or die "open $path: $!";
    local $/;
    return scalar <$fh>;
}

sub spew {
    my ($path, $content) = @_;
    open my $fh, '>', $path or die "open $path: $!";
    print {$fh} $content;
    close $fh or die "close $path: $!";
}

my $json = JSON::PP->new->canonical->pretty;

# ── assertion: require-perl ─────────────────────────────────
if (defined $opt{require}) {
    my ($want_maj, $want_min, $want_mode) = $opt{require} =~
        /^(\d+)\.(\d+)(?:-(threaded|non-threaded|nonthreaded))?$/
        or die "cannot parse --require value: $opt{require}\n";
    $want_mode = 'non-threaded' if ($want_mode // '') eq 'nonthreaded';
    my $ok = ($want_maj == $ver_major && $want_min == $ver_minor)
        && (!defined $want_mode || $want_mode eq $mode_name);
    record('require-perl', $ok ? 'PASS' : 'FAIL',
           "ambient perl is $Config{version} ($mode_name)"
           . ($ok ? "" : ", required $opt{require}"));
    if (!$ok) {
        # 対象 perl が違う状態で先に進んでも全検査が誤爆するだけなので即終了
        print "FATAL: ambient perl does not match --require; aborting\n";
        exit 1;
    }
} else {
    record('require-perl', 'INFO', "ambient perl is $Config{version} ($mode_name), no --require given");
}

# ── perl -V の保存 ──────────────────────────────────────────
run_cmd([$^X, '-V'], stdout => File::Spec->catfile($out_dir, 'perl-V.txt'));

# ── 期待値のロードとマージ ──────────────────────────────────
my $common_file = File::Spec->catfile($opt{expect}, 'common.json');
my $expect = -f $common_file ? $json->decode(slurp($common_file)) : {};
my $overlay_file = File::Spec->catfile($opt{expect}, "v$ver_major.$ver_minor.json");
my $overlay = -f $overlay_file ? $json->decode(slurp($overlay_file)) : {};

# common / overlay ともモード別サブセクション ("threaded" / "non-threaded",
# 同スキーマ) を持てる。マージ順 (後勝ち):
#   common 直下 → common->{当該モード} → overlay 直下 → overlay->{当該モード}
# 例: threaded スナップショット bindings.rs 起因の非 threaded 共通制限は
# common の "non-threaded" に、版固有の失敗は v5.X.json 側に置く。
my @layers = ( $expect, $expect->{$mode_name} || {},
               $overlay, $overlay->{$mode_name} || {} );

my @must_generate = (
    @{ $expect->{must_generate} || [] },
    map { @{ $_->{must_generate_extra} || [] } } @layers,
);
my @must_not_generate = map { @{ $_->{must_not_generate} || [] } } @layers;
my @thx_required = @{ $expect->{thx_required} || [] };
my %bounds = map { %{ $_->{bounds} || {} } } @layers;

%xfail_must_gen = map { ($_ => 1) }
    map { @{ ($_->{known_failures} || {})->{must_generate} || [] } } @layers;
%xfail_assert = map { ($_ => 1) }
    map { @{ ($_->{known_failures} || {})->{assertions} || [] } } @layers;

record('expect', 'INFO', sprintf(
    "common=%s overlay=%s (must_generate=%d, known_failures: %d names, %d assertions)",
    (-f $common_file ? 'loaded' : 'MISSING'),
    (-f $overlay_file ? "v$ver_major.$ver_minor.json" : 'none'),
    scalar @must_generate, scalar keys %xfail_must_gen, scalar keys %xfail_assert));

# ── macrogen の実行 ────────────────────────────────────────
die "macrogen binary not found: $macrogen (run `cargo build` first)\n"
    unless -x $macrogen;

# require リスト = must_generate − known_failures (XFAIL 分は自前 grep で判定)
my @require_list = grep { !$xfail_must_gen{$_} } @must_generate;
my $require_file = File::Spec->catfile($out_dir, 'require-list.txt');
spew($require_file, join("\n",
    "# generated by multi-perl-smoke.pl for $leg (must_generate minus known_failures)",
    @require_list) . "\n");

my $bindings_out = File::Spec->catfile($out_dir, 'macro_bindings.rs');
my $stderr_log   = File::Spec->catfile($out_dir, 'macrogen.stderr.log');
my $status = run_cmd(
    [$macrogen, 'samples/xs-wrapper.h', '--auto', '--gen-rust',
     '--bindings', 'samples/bindings.rs',
     '--require-codegen-list', $require_file,
     '-o', $bindings_out,
     @macrogen_extra_args],
    stderr => $stderr_log,
);
my $exit_code = ($status >> 8);
my $stderr_text = -f $stderr_log ? slurp($stderr_log) : '';

# require-codegen-list 違反のパース ("  NAME - reason" 行)
my %violation;    # 関数名 → 理由
if ($stderr_text =~ /^require-codegen-list violation:/m) {
    while ($stderr_text =~ /^  (\S+) - (.*)$/mg) {
        $violation{$1} = $2;
    }
}

# ── assertion: exit-status ──────────────────────────────────
# require 違反による exit 1 は must-generate 側の検査として扱い、
# それ以外の非 0 終了だけを exit-status の失敗とする
my $only_require_violation = ($exit_code == 1 && %violation) ? 1 : 0;
record_checked('exit-status',
    ($exit_code == 0 || $only_require_violation) && -s $bindings_out,
    "macrogen exited $exit_code (see macrogen.stderr.log)",
    "macrogen ok, output " . (-s $bindings_out || 0) . " bytes");

my $generated = -f $bindings_out ? slurp($bindings_out) : '';

# ── assertion: perl-mode ────────────────────────────────────
my ($reported_mode) = $stderr_text =~ /^\[perl-mode\] (Threaded|NonThreaded)$/m;
my $want_perl_mode = $threaded ? 'Threaded' : 'NonThreaded';
record_checked('perl-mode',
    defined $reported_mode && $reported_mode eq $want_perl_mode,
    "stderr reports [perl-mode] " . ($reported_mode // '(missing)') . ", expected $want_perl_mode",
    "[perl-mode] $want_perl_mode");

# ── assertion: must-generate / must-not-generate ────────────
sub fn_generated {
    my ($name, $text) = @_;
    # 注意: 呼び出し元の引数リスト内 (リストコンテキスト) で使われるので、
    # 失敗時に空リストが返って引数がずれないよう必ずスカラを返す
    return $text =~ /^pub unsafe fn \Q$name\E[(<]/m ? 1 : 0;
}

for my $name (@require_list) {
    record_checked("must-generate/$name",
        !exists $violation{$name} && fn_generated($name, $generated),
        $violation{$name} // "not found in generated output",
        undef);
}
for my $name (sort keys %xfail_must_gen) {
    # known_failures 登録分: require リストから外してあるので自前 grep で判定
    my $ok = fn_generated($name, $generated);
    record("must-generate/$name", $ok ? 'XPASS' : 'XFAIL',
           $ok ? "now generated — remove from known_failures of v$ver_major.$ver_minor.json"
               : "known failure");
}
for my $name (@must_not_generate) {
    record_checked("must-not-generate/$name",
        !fn_generated($name, $generated),
        "unexpectedly generated — remove from must_not_generate of v$ver_major.$ver_minor.json",
        undef);
}

# ── assertion: bounds ──────────────────────────────────────
my %counts = (
    codegen_suppressed  => scalar(() = $generated =~ /^\/\/ \[CODEGEN_SUPPRESSED\]/mg),
    unresolved_names    => scalar(() = $generated =~ /^\/\/ \[UNRESOLVED_NAMES\]/mg),
    cascade_unavailable => scalar(() = $generated =~ /^\/\/ \[CASCADE_UNAVAILABLE\]/mg),
    calls_unavailable   => scalar(() = $generated =~ /^\/\/ \[CALLS_UNAVAILABLE\]/mg),
    generated_fn        => scalar(() = $generated =~ /^pub unsafe fn /mg),
    thx_markers         => scalar(() = $generated =~ /\[THX\]/g),
);
record('counts', 'INFO', join(', ', map { "$_=$counts{$_}" } sort keys %counts));

my @bound_checks = (
    [ 'bounds.codegen_suppressed_max', $counts{codegen_suppressed},
      $bounds{codegen_suppressed_max}, '<=' ],
    [ 'bounds.unresolved_names_max',   $counts{unresolved_names},
      $bounds{unresolved_names_max},   '<=' ],
    [ 'bounds.generated_fn_min',       $counts{generated_fn},
      $bounds{generated_fn_min},       '>=' ],
);
for my $check (@bound_checks) {
    my ($id, $actual, $limit, $cmp) = @$check;
    if (!defined $limit) {
        record($id, 'INFO', "$actual (no bound set — report only)");
        next;
    }
    my $ok = $cmp eq '<=' ? ($actual <= $limit) : ($actual >= $limit);
    record_checked($id, $ok, "$actual violates $cmp $limit", "$actual $cmp $limit");
}

# ── assertion: thx ─────────────────────────────────────────
if ($threaded) {
    # 生成されなかった名前は must-generate 側で報告済みなので、
    # thx 検査は実際に生成された名前の doc 行だけを見る (二重報告を防ぐ)
    my @missing = grep {
        my $n = $_;
        fn_generated($n, $generated)
            && $generated !~ /^\/\/\/ \Q$n\E \[THX\]/m;
    } @thx_required;
    record_checked('thx',
        $counts{thx_markers} > 0 && !@missing,
        $counts{thx_markers} == 0 ? "no [THX] markers in threaded output"
            : "thx_required without [THX] doc line: @missing",
        "$counts{thx_markers} [THX] markers, all thx_required present");
} else {
    record_checked('thx',
        $counts{thx_markers} == 0,
        "$counts{thx_markers} [THX] markers in non-threaded output (expected 0)",
        "no [THX] markers (non-threaded)");
}

# ── assertion: rustfmt-parse ────────────────────────────────
if (which('rustfmt')) {
    my $rustfmt_log = File::Spec->catfile($out_dir, 'rustfmt.log');
    my $st = run_cmd(['rustfmt', '--edition', '2024'],
                     stdin  => $bindings_out,
                     stdout => File::Spec->devnull,
                     stderr => $rustfmt_log);
    record_checked('rustfmt-parse', $st == 0,
        "rustfmt --edition 2024 failed (see rustfmt.log)",
        "output parses");
} else {
    record('rustfmt-parse', 'WARN', 'rustfmt not found — skipped');
}

# ── summary ────────────────────────────────────────────────
my %tally;
$tally{ $_->{status} }++ for @results;
my $failed = ($tally{FAIL} || 0)
    + ($opt{strict} ? ($tally{XFAIL} || 0) + ($tally{XPASS} || 0) : 0);

spew(File::Spec->catfile($out_dir, 'summary.json'), $json->encode({
    leg          => $leg,
    perl_version => $Config{version},
    perl_mode    => $mode_name,
    archname     => $Config{archname},
    macrogen     => $macrogen,
    counts       => \%counts,
    results      => \@results,
    tally        => \%tally,
    ok           => ($failed ? JSON::PP::false : JSON::PP::true),
}));

print "\n== summary ($leg): ",
    join(', ', map { "$_=$tally{$_}" } sort keys %tally),
    " => ", ($failed ? "NG" : "OK"), " ==\n";
print "artifacts: $out_dir\n";

# ── baseline 出力 ──────────────────────────────────────────
if ($opt{baseline}) {
    my @fail_names = sort map { my $n = $_->{id}; $n =~ s{^must-generate/}{}; $n }
        grep { $_->{status} =~ /^(?:FAIL|XFAIL)$/ && $_->{id} =~ m{^must-generate/} }
        @results;
    my @fail_asserts = sort map { $_->{id} }
        grep { $_->{status} =~ /^(?:FAIL|XFAIL)$/ && $_->{id} !~ m{^must-(?:not-)?generate/} }
        @results;
    my $baseline = {
        comment => "baseline recorded from $leg (perl $Config{version});"
            . " bounds = observed + headroom",
        bounds => {
            codegen_suppressed_max => $counts{codegen_suppressed} + 3
                + ceil($counts{codegen_suppressed} * 0.15),
            unresolved_names_max => $counts{unresolved_names} + 3
                + ceil($counts{unresolved_names} * 0.15),
            generated_fn_min => int($counts{generated_fn} * 0.9),
        },
        (@fail_names || @fail_asserts
            ? (known_failures => {
                (@fail_names   ? (must_generate => \@fail_names)  : ()),
                (@fail_asserts ? (assertions   => \@fail_asserts) : ()),
              })
            : ()),
    };
    my $encoded = $json->encode($baseline);
    spew(File::Spec->catfile($out_dir, 'baseline.json'), $encoded);
    print "\n-- baseline fragment (seed for v$ver_major.$ver_minor.json) --\n",
        $encoded;
    exit 0;
}

exit($failed ? 1 : 0);
