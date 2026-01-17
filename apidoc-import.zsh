#!/bin/zsh

function die { echo 1>&2 $*; exit 1 }

realScriptFn=$(readlink -f $0) || die "Can't resolve script path!"
appDir=$realScriptFn:h
dataDir=${realScriptFn%%-import.zsh}

echo dataDir=$dataDir

function usage {
    echo 1>&2 $*;
    cat 1>&2 <<EOF
Usage: ${realScriptFn:t} PERL_GIT_REPO
EOF
    exit 1
}
#----------------------------------------

((ARGC)) || usage "PERL_GIT_REPO is required!"

git_repo=$1; shift

git -C $git_repo tag |
perl -nle '
  /^v5\.(\d+)(\.\d+)?$/ or next;
  next unless $1 % 2 == 0;
  $vers[$1] = $_;
  END { print for grep {defined} @vers }
' |
while read ver; do
    echo $ver;
    git -C $git_repo checkout $ver || die "Can't checkout $ver"
    destFn=$dataDir/$ver:r.json
    cargo run -- --apidoc-to-json $git_repo/embed.fnc \
          -o $destFn || break
done
