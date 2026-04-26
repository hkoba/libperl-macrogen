# 非 threaded perl 対応の動作検証ログ

このドキュメントは非 threaded perl ビルドへの対応 (コミット
1〜4 / `doc/plan/non-threaded-perl-support.md`) の動作確認結果を残す。

## 検証環境

- 非 threaded perl: `tmp/perls/v5.42.2/bin/perl` (`Config{usethreads}=undef`)
- threaded perl: システム標準 `/usr/bin/perl`
- 入力: `samples/xs-wrapper.h` + `apidoc/v5.42.json` + threaded `samples/bindings.rs`

## 検証手順

```bash
# auto-detect (非 threaded 環境)
PATH=$PWD/tmp/perls/v5.42.2/bin:$PATH \
  cargo run --release --quiet -- samples/xs-wrapper.h \
    --auto --gen-rust --bindings samples/bindings.rs \
    --apidoc apidoc/v5.42.json

# 明示指定 (threaded 強制)
PATH=$PWD/tmp/perls/v5.42.2/bin:$PATH \
  cargo run --release --quiet -- samples/xs-wrapper.h \
    --auto --gen-rust --bindings samples/bindings.rs \
    --apidoc apidoc/v5.42.json --perl-build-mode threaded

# 明示指定 (non-threaded 強制、threaded perl 環境でも動作)
cargo run --release --quiet -- samples/xs-wrapper.h \
    --auto --gen-rust --bindings samples/bindings.rs \
    --apidoc apidoc/v5.42.json --perl-build-mode non-threaded
```

## 結果

| シナリオ | 起動ログ | `[THX]` 件数 | コメント |
|---|---|---|---|
| 非 threaded perl + auto | `[perl-mode] NonThreaded` | 0 | 期待通り |
| 非 threaded perl + `--perl-build-mode threaded` | `[perl-mode] Threaded` | 多数 | 強制 override が動く |
| threaded perl + auto | `[perl-mode] Threaded` | 多数 | 既存挙動と一致 |
| threaded perl + `--perl-build-mode non-threaded` | `[perl-mode] NonThreaded` | 0 | 強制 override が動く |

`[THX]` がゼロ件 = `is_thx_dependent` が全マクロ・inline 関数で false に
なっていること、つまり `my_perl: *mut PerlInterpreter` 引数注入と
`my_perl,` 自動挿入のすべての経路が無効化されていることを意味する。

## 既知の制約 (本タスクのスコープ外)

### bindgen 経由の非 threaded `bindings.rs` 生成

非 threaded perl 環境では、`PERLVAR(I, curcop, COP *)` 経由で宣言される
`extern COP * PL_curcop;` 等のインタプリタ変数が、bindgen 0.69.4 での
出力 `bindings.rs` に **抽出されない** 現象を確認。clang の AST には
存在し、preprocessor 出力にも `extern COP * PL_curcop;` として現れるため、
原因は bindgen 側の挙動と思われる。

切り分け試行:
- `--allowlist-file '.*intrpvar\.h'` / `--allowlist-var 'PL_.*'` /
  フィルタ無し、いずれでも非取得
- 単独テストヘッダで `extern COP * PL_curcop;` だけを書けば bindgen は
  `pub static mut PL_curcop: *mut COP;` を出す → bindgen 自体は
  動作する
- perl headers と組み合わせると消える

`libperl-macrogen` 自体は問題なく非 threaded codegen を行えるので、
本タスクのスコープでは解決しない。consumer crate (`libperl-sys` 等)
側で bindgen 設定を調整するか、非 threaded 用 `bindings.rs` を手書きで
生成するパッチ層を入れる必要がある。

### 非 threaded での残存 `[UNRESOLVED_NAMES]`

`samples/bindings.rs` (threaded build から bindgen 生成) を非 threaded
codegen に与えると、`Perl_POPMARK` などが `Unresolved: PL_markstack_ptr,
PL_markstack` として未解決名を抱える。`PL_markstack_ptr` 等は非
threaded bindings.rs にあるべき extern static だが、上記 bindgen 問題
で抽出されず、結果として未解決になる。bindings.rs を非 threaded から
正しく生成すれば解消する見込み。
