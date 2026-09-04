# 進捗ログ（Progress Log）

`docs/implementation-plan.md`の各フェーズについて、実施結果を時系列で記録する。次フェーズに着手する際は、
このログと計画ファイルを読み直してから進める。

フォーマット:
```
## YYYY-MM-DD PX: フェーズ名
- 実施内容: ...
- テスト結果: ...
- 問題・注意点: ...
- 状態: 完了 / 一部完了 / ブロック
```

---

## 2026-09-04 P0: プロジェクト基盤

- 実施内容:
  - Cargoワークスペース作成（`crates/core`=`filechecker-core`、`crates/cli`=`filechecker-cli`）。
    現時点では雛形のみ（core: `version()`のみ、cli: coreのversionを表示するだけのmain）。
  - GitHub Actions CI（`.github/workflows/ci.yml`）を3OSマトリクス（windows-latest / macos-latest /
    ubuntu-latest、Windows Tier-1方針で先頭に配置）で追加。build/test/fmt/clippyを実行。
  - `.gitignore`に`/target`を追加（`Cargo.lock`はバイナリを含むワークスペースのためコミット対象とする）。
- テスト結果:
  - ローカル（Linux）で`cargo build --workspace`成功、`cargo test --workspace`成功（core 1件のunitテスト
    passed）、`cargo run -p filechecker-cli`で`filechecker-cli 0.1.0`を出力することを確認。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`いずれも
    警告なしで通過。
  - CI自体（3OSでのGitHub Actions実行結果）はPRを作成しGitHub上でのCI実行を待って確認する。
- 問題・注意点: 現時点ではロジックがないため実質的なテストカバレッジはない。P1以降で実装が入り次第、
  各フェーズのテストで検証していく。
- 状態: ローカル確認は完了。CI実行結果はPR作成後に確認予定。
