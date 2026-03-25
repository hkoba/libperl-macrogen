#!/usr/bin/tclsh

#========================================
package require cmdline

proc RUN args {
    puts "# $args"
    if {$::opts(n)} return
    =RUN {*}$args >@ stdout
}
proc =RUN args {
    exec -ignorestderr {*}$args 2>@ stderr
}

#========================================

array set opts [cmdline::getoptions ::argv {
    {n "dry-run"}
}]

set outputDir tmp/help

RUN rm -rf $outputDir

RUN mkdir -p $outputDir

RUN perl -Mstrict -wnle {
    m{^\s*   Compiling libperl-sys} .. eof or next;
    next if m{^\Qwarning: `libperl-sys` (build script) generated};
    print;
} {*}$::argv | perl -00 -Mstrict -Mautodie -ne {
    chomp;
    our $outputDir;
    my ($help) = m{\nhelp: ([^\n]+)} or next;
    my ($kind) = m{^(error|warning)(?:\[\w+\])?:} or next;
    our %dict;
    my $firstTime;
    my $dirRec = $dict{$help} //= do {
        $firstTime++;
        +{dirNo => scalar (keys %dict), fileNo => 0};
    };
    my $dirName = "$outputDir/$dirRec->{dirNo}";
    if ($firstTime) {
        mkdir($dirName);
        open my $outFH, '>', "$dirName/__help__.txt";
        print $outFH "($kind) $help\n";
    }
    my $fileNo = ++$dirRec->{fileNo};
    {
        open my $outFH, '>', "$dirName/$fileNo.txt";
        print $outFH $_, "\n";
    }
    print "[$fileNo] $help\n";
} -s -- -outputDir=$outputDir
