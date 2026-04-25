# apidoc データのリリース運用

`apidoc/v5.X.json` および `apidoc/v5.X.patches.json` の更新を下流に届けるまでの
手順をまとめる。背景・スキーマは
[architecture-apidoc-patches.md](architecture-apidoc-patches.md) を参照。

---

## 取り込み経路の全体像

```
apidoc/
  v5.X.json            ← apidoc-import.zsh で perl ソースから自動生成
  v5.X.patches.json    ← 手動メンテ（バグ訂正用）
        ↓
build.rs が tar.gz 化 → OUT_DIR/apidoc.tar.gz
        ↓
src/apidoc_data.rs::EMBEDDED_APIDOC で include_bytes! によりバイナリに埋め込み
        ↓
ランタイム初回呼び出し時に展開:
  ~/.cache/libperl-macrogen/apidoc-v$VERSION/apidoc/
```

`build.rs` には **2 つの取得経路** がある:

| 経路 | 条件 | 動作 |
|------|------|------|
| ローカル | `Path::new("apidoc").is_dir()` が真 | `tar -czf` で apidoc/ を直接固める |
| ダウンロード | ローカルが無い | GitHub Releases `apidoc-v$VERSION` の `apidoc.tar.gz` を ureq で取得 |

`Cargo.toml` の `exclude = ["apidoc/", ...]` により **crates.io publish 時は apidoc/
が除外される** ため、crates.io 経由で利用される場合のみダウンロード経路を踏む。

---

## 利用形態別の必要作業

### 1. **git dependency** で利用する consumer（現在の libperl-sys / libperl-rs）

```toml
# Cargo.toml
[dependencies]
libperl-macrogen = { git = "https://github.com/hkoba/libperl-macrogen.git" }
```

git checkout には `apidoc/` が含まれるので **build.rs はローカル経路を踏む**。

→ **必要な作業: git に commit + push のみ**。下流で `cargo update -p
libperl-macrogen` するだけで反映される。GitHub Release の差し替えは不要。

### 2. **crates.io 公開版** を利用する consumer（将来）

`exclude` で apidoc/ が落ちるので build.rs はダウンロード経路を踏む。

→ **必要な作業:**
1. `apidoc/` を更新して git に commit
2. `apidoc.tar.gz` を作って GitHub Release `apidoc-v$VERSION` に upload

```zsh
# プロジェクトルートで
tar -czf apidoc.tar.gz apidoc/
gh release upload apidoc-v1.0 apidoc.tar.gz --clobber
```

`--clobber` で同タグの既存 asset を置換。

---

## キャッシュ無効化の落とし穴

`src/apidoc_data.rs::APIDOC_DATA_VERSION` は **キャッシュの key**:

```rust
let cache_dir = cache_base
    .join("libperl-macrogen")
    .join(format!("apidoc-v{}", APIDOC_DATA_VERSION));
let version_file = cache_dir.join("version");
if cached_version.trim() == APIDOC_DATA_VERSION {
    return Ok(apidoc_dir);  // キャッシュをそのまま使う
}
```

つまり **`APIDOC_DATA_VERSION` を据え置いたまま中身だけ更新すると、ユーザの
`~/.cache/libperl-macrogen/apidoc-v1.0/` はそのままで更新が反映されない**。

CI 環境（GitHub Actions 等）はキャッシュを跨がないので問題ないが、ローカル開発者
には次のいずれかで手動削除を案内する必要がある:

```zsh
rm -rf ~/.cache/libperl-macrogen/apidoc-v1.0
```

### 破壊的変更のときは version bump

patches.json で挙動が大きく変わる場合や、apidoc スキーマが拡張された場合は、

1. `build.rs:13` の `APIDOC_DATA_VERSION` を `1.1` に bump
2. `src/apidoc_data.rs:15` の同定数も bump
3. リリースタグ `apidoc-v1.1` を新規作成
4. libperl-macrogen 自体の crate version も bump

を推奨。これによりキャッシュディレクトリ名が `apidoc-v1.1` に変わり、自動的に
旧キャッシュが無効化される。

---

## GitHub Actions による自動化

`.github/workflows/release-apidoc.yml` として置く。`apidoc/` 以下に変更が入った
main ブランチ push で自動実行、または `workflow_dispatch` で手動キック。

```yaml
name: Update apidoc release

on:
  push:
    branches: [main]
    paths:
      - 'apidoc/**'
  workflow_dispatch:

jobs:
  upload:
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4

      - name: Pack apidoc.tar.gz
        run: tar -czf apidoc.tar.gz apidoc/

      - name: Upload (or replace) release asset
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          # apidoc-v1.0 タグが無ければ新規作成、あれば clobber で置換
          if gh release view apidoc-v1.0 >/dev/null 2>&1; then
            gh release upload apidoc-v1.0 apidoc.tar.gz --clobber
          else
            gh release create apidoc-v1.0 apidoc.tar.gz \
              --title "apidoc data v1.0" \
              --notes "Perl API documentation data for versions 5.10-5.42"
          fi
```

### 動的にバージョンを取りたい場合

`APIDOC_DATA_VERSION` を src から取り出して使う形にすると、bump 後の自動追従が
できる:

```bash
VER=$(grep -oP 'APIDOC_DATA_VERSION: &str = "\K[\d.]+' src/apidoc_data.rs)
TAG="apidoc-v${VER}"
gh release upload "$TAG" apidoc.tar.gz --clobber 2>/dev/null \
  || gh release create "$TAG" apidoc.tar.gz \
       --title "apidoc data v${VER}" \
       --notes "Perl API documentation data"
```

ただし bump 自体は手動で行う前提（src と Cargo.toml を同時に編集する必要があるため）。

---

## 運用フロー早見表

| 状況 | git push | release upload | version bump |
|------|---------|---------------|-------------|
| `v5.X.patches.json` の patch 1 件追加（git dep のみ利用） | ✓ | 不要 | 不要 |
| 同上（crates.io 利用も視野） | ✓ | ✓（手動 or 自動 workflow） | 不要 |
| 新しい perl 版 `v5.Y.json` 追加（後方互換） | ✓ | ✓ | 不要 |
| patches.json のスキーマ拡張・破壊的変更 | ✓ | ✓ | ✓ |
| キャッシュ仕様の変更 | ✓ | ✓ | ✓ |

---

## 関連ファイル

| ファイル | 役割 |
|---------|------|
| `apidoc-import.zsh` | perl git repo から `v5.X.json` を再生成 |
| `apidoc/v5.X.json` | 自動生成 apidoc データ（`apidoc-import.zsh` で更新） |
| `apidoc/v5.X.patches.json` | 手動メンテのバグ訂正パッチ |
| `build.rs` | tar.gz 化（ローカル）または GH Release ダウンロード |
| `src/apidoc_data.rs` | バイナリ埋め込み・ランタイム展開・キャッシュ管理 |
| `Cargo.toml` の `exclude` | crates.io publish 時の apidoc/ 除外設定 |
| `.github/workflows/release-apidoc.yml` | （未設置）リリース自動化 |
