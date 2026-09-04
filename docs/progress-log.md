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

補足: このセッションの途中で、本ブランチの前回PR（#13、要件定義未決事項クローズ）が既に`main`へ
squash mergeされていたことが判明した。指示（「マージ済みPRのブランチは`main`から作り直す」）に従い、
このP0の2コミットは`origin/main`の上に載せ直し、`git push --force-with-lease`（ユーザー承認済み）で
反映した。以降のコミットはこの再構築後のブランチ上に積む。PRは #14（draft, base: main）として作成し、
CI監視のためsubscribe_pr_activityで購読済み。

## 2026-09-04 P1: ハッシュ計算エンジン（core）

- 実施内容:
  - `crates/core/src/hash/`にハッシュ計算モジュールを追加。
    - `algorithm.rs`: `HashAlgorithm`列挙型（Crc32/Md5/Sha1/Sha256、§10.1）。
    - `multi.rs`: `MultiHasher`（要求されたアルゴリズムだけを保持し、同一チャンクを各ハッシャへ並行update、
      §10.8の「同一チャンクを複数ハッシュに並行updateしファイル再読み込みを避ける」設計をそのまま実装）と
      `HashValues`（各アルゴリズムの結果を`Option`で保持）。
    - `mod.rs`: `hash_reader`（`Read`から任意個のアルゴリズムを1パスで計算）、`compute_crc32`/
      `compute_sha256`（単一アルゴリズムの便利関数、§10.2の遅延パスの構成要素）。
  - 依存クレート追加（`crc32fast`/`digest`/`md-5`/`sha1`/`sha2`、dev-dependencyに`hex`）。
  - 「遅延パス（通常フォルダ）」と「即時全計算パス（リムーバブルメディア）」自体のオーケストレーション
    （§10.3/§10.8の使い分けロジック、DB書き込みタイミング等）はスキャン層の責務のためP3に持ち越し。
    P1では両方のパスが同じ`hash_reader`プリミティブで表現できることの確認にとどめた。
- テスト結果:
  - `cargo test -p filechecker-core`: 7件全てpassed（既知ベクタ: 空文字列・"abc"の4アルゴリズム全て、
    未要求アルゴリズムが`None`のままであること、単体関数がマルチハッシュ結果と一致すること、
    チャンク境界（`DEFAULT_CHUNK_SIZE`±1、複数チャンク）でのハッシュ一貫性、複数ハッシュ同時計算が
    個別計算結果と一致すること）。
  - `cargo test --workspace`・`cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`
    いずれも成功・警告なし。
  - 既知ベクタ定数は独立してPython（`hashlib`/`zlib.crc32`）で再計算しクロスチェック済み（実装作成時に
    SHA-1の"abc"定数へ手動転記ミスが1件あったが、Rust側のテスト失敗で検出し、Pythonでの再計算で正しい値
    ("...cd0d89d"、40桁）に修正した）。
- 問題・注意点: なし。次はP2（SQLite永続化層）。
- 状態: 完了。

## 2026-09-04 P2: SQLite永続化層（core）

- 実施内容:
  - `crates/core/src/db/`に永続化層を追加。
    - `schema.rs`: `docs/requirements.md` §10.12のDDL（11テーブル）を転記した`SCHEMA_SQL`定数と`apply()`。
      §10.20の`reconstruction_run`/`reconstruction_item`（2テーブル）はP11で別途追加する方針を明記。
    - `connection.rs`: `open_in_memory()`（テスト用）/`open(path)`（ファイルDB、`app_setting`テーブルの
      有無でスキーマ未適用かどうかを判定し二重作成を防止）。`PRAGMA foreign_keys=ON`・`journal_mode=WAL`・
      `busy_timeout`を設定（§10.16のGUI/CLI同時アクセス方針）。
    - `models.rs`: CHECK制約のTEXT列に対応する型付きenum（`TargetType`/`HashMode`/`RunStatus`/`FileStatus`/
      `CheckType`/`ResultStatus`）。
    - `repo.rs`: `scan_run`/`scanned_file`/`reference_set`/`reference_file`/`check_run`/`check_run_source`/
      `integrity_check_result`/`duplicate_group`/`duplicate_group_member`のinsert関数群と、
      `list_integrity_results`（ステータスフィルタ付き、GUI/CLIの`--status`フィルタに対応する形）。
  - 依存クレート追加: `rusqlite`（`bundled`機能でSQLiteを同梱、3OSでの挙動を揃える）、devに`tempfile`。
- テスト結果:
  - `cargo test -p filechecker-core`: 24件全てpassed。
    - unitテスト10件（P1の7件＋db::connectionの3件: 全テーブル存在確認、FK有効化確認、ファイルDB再オープン
      時にスキーマが二重作成されないこと）。
    - `tests/constraints.rs`（13件）: `scan_run`のtarget_type別CHECK制約（folder⇄removable_media排他）、
      不明なtarget_type/result_statusの拒否、`scanned_file`の負サイズ拒否・存在しない`scan_run_id`への
      FK違反、`integrity_check_result`の「reference_file_id/scanned_file_idどちらか必須」CHECK、`check_run`の
      check_type別`reference_set_id`要否CHECK、`reference_set.supersedes_reference_set_id`のUNIQUE制約
      （分岐履歴の拒否）、`duplicate_group`のUNIQUE(check_run_id, sha256)、`scanned_file`の
      `ON DELETE CASCADE`確認。
    - `tests/archive_extraction_failure.rs`（1件）: §10.15のシナリオをrepo層で手動構築し検証。正常な
      working.zip配下のエントリはok、展開失敗したbroken.zip配下の参照ファイルは`missing`ではなく`error`
      （`scanned_file_id`は破損アーカイブ自身の行を指し、`detail`にアーカイブのエラーメッセージが入る）、
      走査に一切出てこない参照ファイルは`missing`（`scanned_file_id`がNULL）となることを、
      `list_integrity_results`のフィルタ結果で区別できることを確認。この時点では実際の突合ロジック
      （P5の整合性チェック本体）はまだ存在せず、スキーマ・repo層がこのシナリオを正しく表現できることの
      確認にとどまる。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`成功
    （1件、`ResultStatus::from_str`が標準トレイト`FromStr::from_str`と紛らわしいというclippy指摘があり、
    `parse_str`にリネームして解消）。
  - `cargo test --workspace`成功（P1のhashテスト含め全て通過）。
- 問題・注意点: なし。次はP3（ファイル走査、通常フォルダ）。
- 状態: 完了。

## 2026-09-04 P3: ファイル走査（通常フォルダのみ、core）

- 実施内容:
  - `crates/core/src/retry.rs`: §10.17のファイル単位リトライ方針を汎用化した`retry_io()`。
    権限エラー（`PermissionDenied`）は即失敗、それ以外は200ms→400ms→800msの指数バックオフで
    3回まで再試行（計4回試行）。何を再試行対象とするかは呼び出し側が`is_retryable`で指定する設計とし、
    走査以外（将来のハッシュ計算等）でも再利用できる形にした。`is_retryable_fs_error()`はファイル
    メタデータ/読み取り用のデフォルト分類（権限エラーのみ非対象）。
  - `crates/core/src/scan/mod.rs`: `scan_folder()`——`walkdir`でフォルダを再帰走査し`scan_run`
    （`target_type='folder'`, `hash_mode='lazy'`）を1件作成、各ファイルのパス・サイズ・mtimeを
    `scanned_file`として記録する。§10.3の方針通りハッシュ計算はここでは行わない（メタデータ収集の
    みに留める）。メタデータ取得（`fs::metadata`、I/Oバウンド）は`rayon`で並列化し、DB書き込みは
    1トランザクションにまとめて直列化。個別ファイルの取得エラーは`retry_io`で再試行後、
    `status='error'`として記録して走査全体は継続（§10.17のスキップ＆継続方針）。ディレクトリ自体が
    列挙不能な場合（`walkdir`のエラー）は`scanned_file`化できないため`ScanSummary.walk_errors`で
    件数のみ報告する。
  - 依存クレート追加: `rayon`（並列I/O、§4の性能要件）、`walkdir`（再帰走査）。
- テスト結果:
  - `cargo test --workspace`: 32件全てpassed（P0-P2の28件＋P3の4件: ネストしたフォルダの正常走査、
    空フォルダでの完了、エラーメッセージ分類の権限/その他判定、実際のOSレベル権限エラーでの
    スキップ＆継続——このテストはroot権限などパーミッションビットが効かない環境では自身の前提を
    確認して安全にスキップする）。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`成功
    （2件、`io::Error::new(ErrorKind::Other, ..)`が`io::Error::other(..)`推奨というclippy指摘が
    `retry.rs`のテストと`scan/mod.rs`のテストにあり、両方修正して解消）。
- 問題・注意点: なし。次はP4（重複チェック、アーカイブ抜き）。
- 状態: 完了。
