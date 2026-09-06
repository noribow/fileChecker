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

## 2026-09-04 P4: 重複チェック（アーカイブ抜き、core）

- 実施内容:
  - `crates/core/src/duplicate/mod.rs`: `run_duplicate_check()`——`scan_run_id`（複数可、§3.2の
    「複数フォルダを横断」）の`scanned_file`（`status='ok'`かつ非アーカイブ内エントリのみ）を対象に、
    §10.2の段階的フィルタ（サイズ→CRC32(全体)→SHA-256(全体)）で重複グルーピングを行う。各段階は
    `HashMap`でグルーピングしメンバーが1件だけの組は次段階に進めず即座に脱落させる（無駄なハッシュ計算・
    I/Oを避ける、§10.2の設計意図通り）。ハッシュ計算自体は`rayon`で段階内並列化。最終的な
    `duplicate_group`は`sha256`のみをキーにグルーピングする（`duplicate_group`テーブルの
    `UNIQUE(check_run_id, sha256)`と整合させるため。内容が同一ならサイズも必然的に同一なので問題ない）。
  - `check_run`（`duplicate`種別）を1件作成し、渡された`scan_run_id`群を`check_run_source`として記録。
    最終的な組を`duplicate_group`/`duplicate_group_member`としてトランザクション内で挿入する。
  - §10.11のエラー区別を比較フェーズにも適用: 比較フェーズでのハッシュ計算失敗（走査後にファイルが
    削除・読み取り不能になった等）は`scanned_file.status='error'`へ更新（新設の
    `repo::mark_scanned_file_error`）した上でグルーピングから除外し、`error_count`として明示的に返す
    （黙って無視しない）。走査時点で既に`status='error'`だったファイルは対象クエリの時点で除外。
    ファイル読み取りには`retry.rs`の`retry_io`（§10.17と同じリトライ方針）を再利用。
  - `db::repo`に追加: `list_ok_scanned_files_for_scan_runs`（複数`scan_run_id`をIN句で束ね、
    `scan_run.folder_path`とJOINしてフルパス解決に使う）、`update_scanned_file_crc32`/
    `update_scanned_file_sha256`（計算したハッシュ値を`scanned_file`に永続化。今回はグルーピングにしか
    使わないが、値自体はP5の整合性チェックでも再利用できる）、`mark_scanned_file_error`。
  - 依存クレートの追加なし（P1の`hash_reader`系・P3の`retry`/`rayon`をそのまま再利用）。
- テスト結果:
  - `cargo test --workspace`: 36件全てpassed（P0-P3の32件＋P4の4件: 2フォルダ横断での重複グルーピング
    正しさ、サイズ一致but内容不一致（CRC32/SHA256不一致）が非グルーピングとなること、比較フェーズでの
    読み取り不能ファイルがグルーピングから除外されつつ`error_count`に計上され`scanned_file.status`が
    `error`に更新されること（Unix権限ビットで実際のI/Oエラーを再現、rootなど権限ビットが効かない環境
    では安全にスキップ）、走査時点で既に`error`だったファイルが比較フェーズのクエリから除外され
    サイズグループが1件になった結果ハッシュ計算自体が走らないこと）。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`いずれも
    成功・警告なし。
- 問題・注意点: なし。次はP5（お手本セット＋整合性チェック、アーカイブ抜き）。
- 状態: 完了。

## 2026-09-04 P5: お手本セット（自前JSON）＋整合性チェック（アーカイブ抜き、core）

- 実施内容:
  - `crates/core/src/hash/mod.rs`にファイルレベルのヘルパーを追加: `hash_file`/`hash_file_crc32`/
    `hash_file_sha256`——`File::open`＋`retry_io`（§10.17と同じリトライ方針）＋既存の`hash_reader`系を
    束ねたもの。これまでP4（重複チェック）内にプライベート関数として重複していた「パスを開いて
    リトライ付きでハッシュ計算する」ロジックを、この後P5で新たに2箇所（お手本セット生成・整合性
    チェック）から必要になるタイミングで`hash`モジュール側の共通APIとして一本化し、`duplicate/mod.rs`
    もこちらを使うようリファクタ。
  - `crates/core/src/reference/mod.rs`: `generate_reference_set_from_scan_run()`——CLI仕様
    （`reference generate --from-scan <SCAN_RUN_ID>`、§10.16）通り、既存の`scan_run`（P3のメタデータのみ
    走査結果）を入力にSHA-256を計算し、`reference_set`（`source_format='json'`,
    `generated_from_scan_run_id`設定）＋`reference_file`群（SHA-256のみ、§10.1の標準アルゴリズム）を
    生成する。`supersedes_reference_set_id`を引数で受け取り、バージョン連鎖に対応（§10.12）。
    比較フェーズでのハッシュ失敗ファイルは§10.11の方針通り`scanned_file`をerror化し生成対象から除外
    （黙って無視しない）。
  - `crates/core/src/integrity/mod.rs`: `run_integrity_check()`——`reference_set`と1つ以上の`scan_run`
    の`scanned_file`をパスでインデックスして突合し、§10.11の5ステータス（ok/corrupted/missing/extra/
    error）を判定して`integrity_check_result`に記録する。
    - 走査時点で既に`status='error'`だったファイルが参照セットのパスと一致する場合は、ハッシュ計算を
      試みず直接`error`として記録（`missing`と誤認しない）。
    - `scanned_file.sha256`が既に永続化済み（P4の重複チェック等で計算済み）ならファイルを再度読まず
      その値を再利用して比較。未計算の場合のみこのフェーズでSHA-256を計算（`rayon`で並列化）し、
      計算失敗はP4と同様`scanned_file`をerror化＋`error`として記録。
    - 参照セットに一致しないパスは`extra`、走査に一件も現れなかった参照エントリは`missing`として記録。
    - `check_run`（`integrity`種別）を作成し`check_run_source`で対象`scan_run_id`群を記録。
    - §10.12の「経年変化検知（T1→T2）」は、`reference generate`でT1の`scan_run`から`reference_set`を
      生成し、T2に同じフォルダを再走査した`scan_run`をこの関数に渡すだけで実現でき、専用の追加ロジックは
      不要（設計通り）であることをテストで確認。
  - 依存クレートの追加なし。
- テスト結果:
  - `cargo test --workspace`: 41件全てpassed（P0-P4の36件＋P5の5件）。
    - `reference::tests`（2件）: スキャン結果からのSHA-256付き`reference_file`生成の正しさ、
      `supersedes_reference_set_id`によるバージョン連鎖。
    - `integrity::tests`（3件）: 経年変化検知シナリオ（T1でお手本セット生成→ファイル改変・削除・追加
      →T2再走査→比較）でok/corrupted/missing/extraが正しく作り分けられること、参照セットに一致する
      ファイルが読み取り不能な場合に`missing`でも`corrupted`でもなく`error`として記録されること
      （Unix権限ビットで実際のI/Oエラーを再現、rootなど権限ビットが効かない環境では安全にスキップ）、
      `scanned_file.sha256`が既に永続化済みの場合はファイルを再読み込みせずその値で比較すること
      （意図的に実ファイルと異なる偽のハッシュ値をDBに仕込み、比較結果がその偽の値に基づくことで検証）。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`いずれも
    成功・警告なし。
- 問題・注意点: なし。次はP6（CLI、アーカイブ・リムーバブル抜き）。
- 状態: 完了。

## 2026-09-04 P6: CLI（アーカイブ・リムーバブル抜き）

- 実施内容:
  - `crates/core/src/db/repo.rs`にCLI表示用のクエリ関数群を追加: `list_reference_sets`/
    `get_reference_set`/`reference_set_version`（`supersedes_reference_set_id`連鎖を後方に辿って
    バージョン番号を算出）、`list_check_runs`/`get_check_run`、`list_duplicate_groups`/
    `list_duplicate_group_members`、`app_setting`のCRUD（`get_app_setting`/`list_app_settings`/
    `set_app_setting`）。既存の`IntegrityResultRow`には`path`（scanned優先、なければreference側）と
    `size`のCOALESCE列を追加（既存フィールドはP2/P5のテストが使うため保持）。
  - `crates/cli/`: `clap`（derive）でサブコマンド体系を実装。
    - `scan folder <PATH> [--rescan]`（`--rescan`は引数として受理するが現状常に新規スキャンのため無効。
      「既存scan_runの再利用」判定ロジック自体が未実装なため — 今後の課題として明記）。
    - `reference generate --from-scan <ID> --name <NAME> [--supersede <ID>]` / `reference list`。
    - `check integrity --reference-set <ID> (--folder <PATH> | --scan-run <ID>...)` /
      `check duplicate (--folder <PATH> | --scan-run <ID>)...`（複数指定可、スキャン新規実行と
      既存scan_run再利用を混在可）。
    - `check list [--type integrity|duplicate] [--limit N]` / `check show <ID>`。
    - `report export <ID> --format csv|json --output <FILE>`（`--format html`/`text`は
      「report exportはcsv|jsonのみ対応（textはcheck showを使用）」としてコード64で拒否。HTML出力は
      P13で追加予定のため未実装）。
    - `config get [KEY]` / `config set <KEY> <VALUE>`（`app_setting`直接操作）。
    - 出力仕様（§10.16）: 結果はstdout、進捗・状態はstderr（`--quiet`で抑止）。`--format text|json|csv`
      （既定text）、`--output <FILE>`。`--status`で明細を絞り込み（既定はok以外全件、`--status ok`
      明示時のみok明細も出力）。件数集計（サマリ）は常に`--status`フィルタの影響を受けず全件を反映。
    - 終了コード（§10.16）: `exit.rs`に0/1/2/3/64を定義（4は本フェーズで到達するパスワード関連機能が
      まだ無いため予約のみ）。`integrity_exit_code`/`duplicate_exit_code`でerror>0を最優先、次に
      diff（corrupted/missing/extra、または重複グループ）、`--exit-zero-on-diff`はコード1のみ0に
      読み替え。引数解析失敗はclapのデフォルト終了コード(2)ではなく§10.16のコード64に上書き
      （`Cli::try_parse()`のエラーを捕捉し独自にexit）。
  - 簡略化した点（進捗優先のための意図的なスコープ縮小、将来フェーズで補完予定）:
    - TTY動的プログレス表示は未実装。常に非TTY相当（フェーズ開始・完了を1行ずつstderr出力）の
      表示のみ提供。`--quiet`のみ実装。
    - `--expand-archive-errors`・アーカイブエラー集約表示は未実装（アーカイブ自体がP7未着手のため）。
    - `check show`/`report export`（結果の再表示系）は§10.16の差分ベース終了コード(0/1/2)を適用せず
      常に成功(0)/失敗(3)のみとした。理由: 重複チェックの`error_count`（ハッシュ計算エラー件数）は
      `check_run`単位で永続化されておらず、過去の`check_run`を再表示する際に正確に復元できないため
      （`run_duplicate_check`が返す値としてのみ存在し、DBに列がない）。整合性チェック側はデータ的に
      復元可能だが、挙動を両コマンドで統一するためこの制限を優先した。CI等で終了コードに依存する
      用途は`check integrity`/`check duplicate`の実行時終了コードを直接使う想定。
    - CLI用DBパス（`--db`）はGUIの「アプリ設定フォルダ」既定パスのような自動解決を持たず、常に
      明示指定必須とした（要件定義に既定パスの決定がないため）。
  - テスト戦略の注記: 実装計画の「テスト戦略まとめ」表は CLI テストに`insta`（スナップショット）を
    挙げているが、`insta`依存は追加せず、`assert_eq!`による完全一致の文字列比較（実質的に同じ狙い:
    出力全体を既知の期待値と比較）で代替した。非対話環境でのスナップショット承認フローを避けるため。
  - 依存クレート追加: `clap`（derive機能）・`serde_json`（cli限定、出力整形用）、devに`tempfile`（cli）。
- テスト結果:
  - `cargo test --workspace`: 47件全てpassed（P0-P5の41件＋CLI統合テスト6件、`crates/cli/tests/cli.rs`）。
    - `exit_code_table_is_covered`: 0/1/2は本文中に統合、3（reference-set不在・scan-run不在）・64
      （必須引数欠落・`--folder`と`--scan-run`同時指定）・`--exit-zero-on-diff`による1→0読み替えを検証。
    - `unreadable_matched_file_yields_exit_code_2`: 実際のUnix権限エラーでコード2を確認（root等で
      権限ビットが無効な環境では安全にスキップ。実行環境では実際にpassし、権限ビットが機能することを
      確認できた）。
    - `duplicate_check_exit_codes_and_text_summary`: 重複あり→1・重複なし→0・引数不足→64。
    - `json_and_csv_output_match_expected_snapshots`: `check integrity --format csv`の完全一致
      （5ステータスの作り分けを含む経年変化シナリオ）、`check show --format json --status corrupted`
      でサマリが全件集計を保ちつつ明細のみ絞り込まれることを確認。
    - `report_export_rejects_text_format`・`config_get_and_set_round_trip`。
  - 手動スモークテスト: `scan folder`→`reference generate`→`reference list`→
    `check integrity`（text/json/csv、`--folder`と`--scan-run`双方の経路、`--exit-zero-on-diff`）→
    `check list`/`check show`→`check duplicate`（複数`--folder`）→`report export`→`config get/set`の
    一連の実コマンド実行で最終出力・終了コードを目視確認済み。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`いずれも
    成功・警告なし。
- 問題・注意点: 上記「簡略化した点」を参照。次はP7（アーカイブ対応、zip/7z読み取り）。
- 状態: 完了。

## 2026-09-04 P7: アーカイブ対応（zip/7z読み取り）

- 実施内容:
  - 依存クレート追加: `zip`（`zstd`/`deflate`機能）、`sevenz-rust2`（既定機能＋`zstd`機能）。両クレートとも
    zstd圧縮バリアントの読み取りに対応していることを、実際にzstd圧縮したzip/7zを生成→読み取る往復テストで
    確認済み。
  - `crates/core/src/archive/mod.rs`（新規）: zip/7zの低レベル読み取りAPI。
    - `ArchiveFormat::detect`（拡張子ベース、大文字小文字無視）、`list_entries`（エントリ名・宣言サイズの
      列挙のみ、展開なし——走査/情報取得フェーズにふさわしいメタデータのみの操作）、`read_entry_bytes`
      （1エントリの展開、§10.6の宣言サイズ検査込み）。
    - §10.6の宣言サイズ検査はzip/7zで非対称: zipは`ZipFile: Read`をチャンク単位で読みながら実際のバイト数が
      宣言サイズを超えた時点で即エラーにする真のストリーミング中断（大きい正当なエントリもメモリに載せ
      きらず安全）。7zは`sevenz-rust2`の`ArchiveReader::read_file`が全体展開しか提供しないため、展開完了後に
      `bytes.len()`と宣言サイズを比較する事後チェックになる（非常に大きい正当な7zエントリは全体がメモリに
      載る）。この非対称性はモジュールdocコメントに明記。
    - `ArchiveConfig::from_settings`: `app_setting`の`archive_max_depth`（既定3）・
      `archive_entry_size_limit_bytes`（既定2TiB=2199023255552、schema.rsのコメントが元々想定していた
      キー名と一致）を読み取る。
    - `ScannedEntry`トレイト＋`resolve_hops`＋`hash_entry`: `scanned_file.parent_archive_file_id`を
      （追加のDB問い合わせなしで）遡ってルートの実ファイルパス＋アーカイブ経由チェーンを組み立て、
      末端エントリをハッシュ計算する共通ロジック。`repo::ScannedFileForDuplicate`/`ScannedFileForIntegrity`
      の両方に`ScannedEntry`を実装し、重複チェック・整合性チェック・お手本セット生成の3箇所で共用。
    - `crate::hash`に`hash_file`/`hash_file_crc32`/`hash_file_sha256`（パスを開いて§10.17のリトライ込みで
      ハッシュ計算する共通ヘルパー）を追加し、P4で重複チェック内にプライベート実装されていた同等ロジックを
      置き換え。
  - `crates/core/src/scan/archive_walk.rs`（新規）: 走査フェーズでのアーカイブ再帰展開。
    - トップレベルの実ファイルがzip/7zの拡張子を持てば`archive_format`を設定し（展開の成否に関わらず、
      §10.5の「archive_format列はフォーマット識別のためのもの」という位置づけを反映）、深さ1から再帰的に
      エントリを`scanned_file`として記録する。
    - 展開失敗（アーカイブが開けない・パース不能）は§10.15通り親の`scanned_file.status='error'`に記録し
      子エントリは一切作らない。個別エントリの宣言サイズが上限を超える場合は§10.6の深さ上限超過と同じ
      「それ以上展開せず通常ファイルとして扱う」方針を適用（`archive_format`自体は識別目的で保持するが、
      子エントリは作らない）。
    - ネストしたアーカイブはメモリ上のバイト列（`Vec<u8>`）として保持し、そこから`ZipArchive`/
      `ArchiveReader`を`Cursor`経由で開いて再帰する（Seek要件のため）。深さごとにアーカイブを都度開き直す
      設計（バッチキャッシュなし）。
  - `crates/core/src/duplicate/mod.rs`・`crates/core/src/integrity/mod.rs`・`crates/core/src/reference/mod.rs`
    を更新し、`repo::list_ok_scanned_files_for_scan_runs`/`list_scanned_files_for_integrity`の
    `parent_archive_file_id IS NULL`フィルタを撤廃（§3.3「整合性チェック・重複チェックの対象には...
    圧縮ファイル内部のファイルも含める」）。3モジュールとも直接のファイルパスオープンをやめ、
    `archive::resolve_hops`+`archive::hash_entry`経由のハッシュ計算に統一。
    - `integrity/mod.rs`に§10.15のロジックを実装: 展開失敗した`archive_format`付きerrorな`scanned_file`
      行を`failed_archives`として収集し、走査に一件も現れなかった参照エントリ（`missing`候補）のうち
      パスがいずれかの失敗アーカイブの「パス+"/"」で始まるものを`missing`ではなく`error`として記録
      （`scanned_file_id`は失敗アーカイブ自身の行、`detail`はそのエラーメッセージ）。
  - `db::repo`更新: `ScannedFileForDuplicate`/`ScannedFileForIntegrity`に`parent_archive_file_id`/
    `archive_format`列を追加（`Clone`導出も追加、複数箇所でid付きマップを構築するため）。
- テスト結果:
  - `cargo test --workspace`: 60件全てpassed（core 54件＋CLI 6件）。
    - `archive::tests`（6件）: zip（store/deflate/zstd）・7z（既定LZMA2/zstd）の列挙・展開往復、
      宣言サイズ偽装検出（zip・7z双方）、拡張子判定、破損アーカイブのopen失敗。
    - `scan::archive_walk::tests`（4件）: 正常展開、3階層境界（4階層目の内容が発見されないこと）、
      破損アーカイブのerror記録＋子なし、宣言サイズ上限超過エントリの「記録するが展開しない」動作。
    - `duplicate::tests`に1件追加: 平文ファイルとアーカイブ内エントリの内容一致がグルーピングされること。
    - `integrity::tests`に2件追加: §10.15の実シナリオ（破損アーカイブ配下の期待エントリがmissingでは
      なくerrorになること、実際に`scan_folder`→`run_integrity_check`を通して確認）、アーカイブ内エントリの
      改ざん検知（corrupted判定）。
    - `reference::tests`は既存2件がそのままpass（アーカイブ対応後もリグレッションなし）。
  - CLIバイナリでの手動確認: 実際にPythonで生成したzip（`plain.jpg`と`album.zip/b.jpg`が同一内容）を
    `scan folder`→`check duplicate`→`reference generate`→`check integrity`の一連の実コマンドで確認し、
    重複グループ化・整合性チェック双方がアーカイブ内エントリを正しく扱うことを目視確認。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`いずれも
    成功・警告なし。
- 問題・注意点:
  - 「アーカイブ展開失敗時の集約表示」（§10.14/§10.16、折りたたみ行でのグルーピング表示）自体はCLI/GUI
    表示層の仕事であり、P6で`--expand-archive-errors`同様に未実装のまま据え置いた。DB側は集約に必要な形
    （同一`scanned_file_id`＋同一`detail`を持つ複数の`error`行）で正しく記録されることをテストで確認済み
    なので、表示層の実装（P13想定）はブロックされない。
  - 7zの宣言サイズ検査が事後チェックになる非対称性、アーカイブの都度再オープン（バッチキャッシュなし）は
    いずれもP14の性能検証で問題になれば見直す。
- 状態: 完了。次はP8（リムーバブルメディア識別＋eagerハッシュモード）。

## 2026-09-04 P8: リムーバブルメディア識別＋eagerハッシュモード

- 実施内容:
  - 依存クレート追加（core）: `serde`（`derive`機能）・`serde_json`（`lsblk -J`のJSON出力パース用）。
  - `crates/core/src/media/mod.rs`（新規）: `MediaIdentifier`トレイト（`list_connected() ->
    Vec<DetectedMedia>`）によるOS別識別ロジックの抽象化（§10.4）。
    - Linux実装（`LinuxMediaIdentifier`, `#[cfg(target_os = "linux")]`）: `lsblk -J -o
      NAME,SERIAL,UUID,MOUNTPOINT,RM`を実行しJSONをパース。リムーバブル（`rm=true`）かつマウント済みの
      デバイスについて、祖先ディスクのSERIAL（デバイス単位で安定、§10.4の例示通り優先）→パーティション
      自身のUUID、の順で識別子を採用。どちらも取得できないマウント済みリムーバブルデバイスは意図的に
      「識別不能」として除外（黙って推測しない——呼び出し側が§10.21のフォールバックに委ねる）。`lsblk`
      コマンド自体が存在しない・失敗する場合も同様に「識別不能」として扱う（ハードエラーにしない）。
    - Windows/macOS: プレースホルダ実装（常に空リストを返す）。§10.4が「各OSでどの識別子をどの優先順で
      取得するかは各OS実装時に定める」と明記している通り、実機で検証できていない現時点では「識別子を
      一切取得できない」という安全側の扱いとし、誤った識別子を捏造するリスクを避けた（§6の「再接続なし
      再利用」はこの識別子が物理的に正しいことに依存するため、誤検知は空振りより悪い）。
    - `parse_lsblk_json`はOS非依存の純粋関数として分離し、3OS全てのCIで実行・検証されるようにした
      （実際に`lsblk`を呼び出すのはLinux実装のみ）。
  - `db::repo`に`removable_media`テーブルのCRUDを追加: `find_or_create_removable_media`（
    `UNIQUE(platform, identifier_type, identifier_value)`を使った同一メディアの再認識＋`last_seen_at`
    更新。表示名は新しい値がある場合のみ上書きし、ラベルなしでの再接続で既存の表示名を消さない）、
    `get_removable_media`、`list_removable_media`。
  - `crates/core/src/scan/removable_media.rs`（新規）: `scan_removable_media()`——§10.8のeagerモード。
    通常フォルダの遅延パスと異なり、ファイルごとにCRC32・SHA-256をこの接続中の1パスで計算し
    `scanned_file`に保存する（`scan_run.hash_mode='eager'`）。アーカイブ構造の列挙自体は通常フォルダと
    同じ`archive_walk::expand_if_archive`をそのまま再利用。
  - CLI: `media list`（既知メディア一覧）、`scan media (--media-id <ID> | --mount <PATH>)`。
    - `--media-id`: 既存の`removable_media`行を引き、識別バックエンドで現在接続中のメディア一覧と
      識別子が一致するものを探して接続中と確認できた場合のみスキャン。接続されていなければコード3。
    - `--mount`: 指定パスが接続中メディア一覧のマウントポイントと一致すれば自動識別、一致しなければ
      §10.21のフォールバック——標準入力がTTYならラベル入力を促し（`identifier_type='user_defined'`）、
      TTYでなければコード4で失敗（要件通り、対話待ちでブロックしない）。
- テスト結果:
  - `cargo test --workspace`: 73件全てpassed（core 46+1+13+3件＋CLI 10件）。
    - `media::tests`（5件）: lsblk JSON解析——ディスクのSERIAL優先、SERIAL欠如時のUUIDフォールバック、
      非リムーバブル/未マウントデバイスの除外、識別子が全く取得できないリムーバブルデバイスの除外
      （フォールバックへ委ねる）、不正なJSON入力での空リスト返却（クラッシュしないこと）。
    - `tests/removable_media.rs`（3件、新規）: 同一識別子での再接続が同一`removable_media`行を再利用し
      `last_seen_at`が更新されること（表示名はラベルなし再接続で消えないこと）、異なる識別子は別メディア
      になること、§10.21のフォールバックラベル自体も同じUNIQUE制約でマッチング・別ラベルは別メディア
      扱いになることの確認。
    - `scan::removable_media::tests`（1件）: eagerモードで`scan_run.hash_mode='eager'`・
      `scanned_file.crc32`/`sha256`がスキャン時点で既に埋まっていることを確認。
    - CLI統合テスト4件（新規）: `media list`の初期空表示、`scan media`の引数必須チェック（コード64）、
      未知メディアIDでのコード3、`--mount`で自動識別できない場合の非TTY環境でのコード4
      （このサンドボックス自体に接続中のリムーバブルメディアが存在しないため、実際に`lsblk`を実行して
      「識別不能」経路を自然に検証できた）。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`いずれも
    成功・警告なし。
- 問題・注意点: 上記「簡略化した点」を参照（Windows/macOS識別バックエンド未実装・未検証、eagerモードは
  トップレベルファイルのみ）。次はP9（外部お手本セット取り込み、MAME形式）。
- 状態: 完了。

## 2026-09-04 P9: 外部お手本セット取り込み（MAME形式）

- 実施内容:
  - 依存クレート追加（core）: `quick-xml`（XMLパーサ）。このバージョンは`QName`が`&str`ベースAPI
    （旧来のbytes版quick-xmlとは異なる）である点に留意して実装。
  - `crates/core/src/import/mod.rs`（新規）: §10.18で確定したMAME向けアダプタ。
    - `MameFormat`（`mame-softwarelist`/`mame-machinelist`、§10.18ポイント8通り形式ごとに独立した
      マッピングテーブル＝別々のパーサ関数として実装）。
    - `softwarelist.dtd`パーサ（`parse_softwarelist`）: `<software><part><dataarea><rom/></dataarea>
      <diskarea><disk/></diskarea></part></software>`を辿り、`{software@name}.zip/{rom@name}`形式の
      pathを構築（merge/romof概念はこの形式に存在しないため常にこの単純形）。
    - `mame.dtd`パーサ（`parse_machinelist`）: `<machine name= romof= isdevice=><rom/><disk/></machine>`
      を1パスで走査し、エントリ一覧と全machineの`romof`マップを同時に構築（マージ解決には全machine分の
      romof情報が事前に要るため）。
    - 除外ロジック（`is_excluded`、§10.18確定分をそのまま反映）: `loadflag`が
      fill/reload/continue/ignore、`status=nodump`、`status=baddump`（既定除外、`--include-baddump`で
      救済）、`machine@isdevice=yes`、`disk@writeable`/`@writable=yes`、`rom@bios`存在、
      `rom@optional=yes`。両形式共通ルールとして1箇所にまとめ、各形式のパーサ側は自形式に存在しない
      属性を単に設定しない（＝該当ルールが発火しない）ことで安全に共有。
    - merge/split選択（`resolve_path`、§10.18ポイント3）: splitは`merge`属性を無視し常に
      `{machine@name}.zip/{rom@name}`。mergedは`merge`属性を持つエントリについて、そのmachineの
      `romof`が指す親machine名を使い`{親machine名}.zip/{merge属性値}`に解決。
    - **実装中に発見したバグと対応**: mergedモードでは、親機種自身の実体ROM（`{parent}.zip/{name}`）と
      クローン側の`merge`解決結果が同一pathに収束するケースが実際に発生する（クローンが親と同じ
      ファイルを共有している、というmerge属性の本来の意味からして当然）。素朴に両方を`reference_file`
      へINSERTすると`UNIQUE(reference_set_id, path)`制約違反でエラーになることをテストで検出。
      対策として、解決済みpathの集合を保持し、2回目以降の同一pathエントリは黙って重複除外
      （`excluded_count`に計上）するよう修正。MAMEデータファイルは通常「親machine→クローン」の順で
      記載されるため、実質的に親machine自身の実体エントリが優先的に採用される。
    - `md5`/`sha256`は両DTDに相当項目が無いため常にNULL（§10.12のNULL許容設計をそのまま利用）。
  - CLI: `reference import --file <FILE> --format mame-softwarelist|mame-machinelist --name <NAME>
    [--merge-mode merged|split] [--include-baddump]`。`mame-machinelist`では`--merge-mode`必須
    （§10.18ポイント3の「自動判定は行わない」方針をそのままCLI引数必須化として反映、未指定はコード64）。
- テスト結果:
  - `cargo test --workspace`: 81件全てpassed（core 50+1+13+3件＋CLI 14件）。
    - `import::tests`（4件、新規）: `docs/外部形式マッピング案.md`の記述を元にしたゴールデンXML
      サンプルで、softwarelist形式の除外網羅（loadflag=reload/fill、nodump、baddump既定除外、
      writeable disk、正常なrom/diskの取り込み）、`--include-baddump`オプションでの救済、
      machinelist形式のsplit/mergedモードでのpath解決（bios/optional/isdeviceの除外、mergedモードでの
      重複解決含む）を確認。
    - CLI統合テスト4件（新規）: `reference import`の正常系（`reference list`への反映まで確認）、
      不明な`--format`（コード64）、`mame-machinelist`での`--merge-mode`未指定（コード64）、
      存在しない入力ファイル（コード3）。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`いずれも
    成功・警告なし。
- 問題・注意点: MAME以外の外部形式（CSV等）は§10.18の決定通りスコープ外（次バージョン以降）。
  次はP10（パスワード保護アーカイブ＋マスターパスワード）——ここからはArgon2id等の暗号実装が入るため、
  着手前に一度状況を整理する。
- 状態: 完了。

## 2026-09-04 P10: パスワード保護アーカイブ＋マスターパスワード

- 実施内容:
  - 依存クレート追加（core）: `argon2`（KDF）・`aes-gcm`（登録パスワード設定ファイルの暗号化）・
    `uuid`（登録パスワードIDの生成）・`zeroize`（導出鍵をメモリ上でゼロ化）。`zip`の`aes-crypto`
    機能（既定で有効）でZipCrypto/AES256暗号化エントリの読み取りに対応済みだったことを確認。
  - `crates/core/src/secrets/mod.rs`（新規）: `UnlockedStore`——登録パスワード設定ファイル
    （§10.9）の読み書きとマスターパスワード（§10.10）のライフサイクル一式。
    - ファイル形式: プレーンテキストの envelope（KDFソルト・Argon2idパラメータ・verifier・
      nonce）でAES-256-GCM暗号文（`Vec<RegisteredPassword>`のJSON）を包む1ファイル。
    - **verifierと暗号鍵は意図的に別々のソルトから導出**（モジュールdocコメントに理由を明記）:
      同一ソルトを使うと、平文で保存されているverifier（Argon2id PHC文字列）から暗号鍵そのものを
      復元できてしまう（マスターパスワードなしで）ため、2つの独立したArgon2id呼び出しに分離した。
    - `create`（既存ファイルがあれば`AlreadyExists`）・`unlock`（誤マスターパスワードは
      `WrongMasterPassword`、改ざん検知はAES-GCMの認証タグ失敗で`Corrupt`）・`add`/`remove`/`list`・
      `save`（アトミック書き込み、tmpファイル+rename）・`change_master_password`（§10.10のマスター
      パスワード変更）・`reset`（§10.10の「リセット」操作、ファイル不在でもエラーにしない）。
    - `archive::PasswordCandidates`トレイトを実装（format別登録パスワードを先に、全形式共通の
      登録パスワードを後に返す）。
  - `crates/core/src/archive/mod.rs`: `PasswordPolicy`（`Reject`=モード1 / `TryRegistered`=モード2）
    と`PasswordCandidates`トレイトを新設。`list_entries`/`read_entry_bytes`/`hash_entry`に
    `policy`引数を追加し、zipは`by_name`→`by_name_decrypt`のフォールバック、7zは
    パスワードごとに`ArchiveReader`を作り直すリトライループ（7zは全体再暗号化のため、ヘッダー
    open自体もパスワード必須になり得る）で実装。
    - **実装中に発見したバグ**: `list_entries`のzip列挙が`archive.by_index(i)`を使っており、
      これは中身の暗号化状態に関わらず「パスワード必須」チェックを通ってしまうため、暗号化
      エントリを含むzipの列挙自体が（本来central directoryのメタデータだけで完結するはずが）
      失敗していた。`by_index_raw`（メタデータのみ、復号なし）に差し替えて解消。CLI統合テストで
      「暗号化zipをスキャン→reference generateでハッシュエラーになる」という結線全体を確認する
      過程で発覆（コアのarchive単体テストだけでは、列挙とハッシュ計算を同じCursorに対して別々に
      呼んでいたため気づけなかった）。
  - 既存の`scan_folder`/`scan_removable_media`/`run_duplicate_check`/`run_integrity_check`/
    `generate_reference_set_from_scan_run`は元のシグネチャのまま維持し（§10.7を意識しないP0-P9の
    全テストが無改修で通る）、`_with_password_policy`版を追加してpolicyを明示的に渡せるようにした
    （デフォルト実装は内部で`PasswordPolicy::Reject`を渡すだけの薄いラッパー）。
  - CLI: グローバル引数`--password-store <PATH>`・`--no-archive-password`を追加。
    `crates/cli/src/password_policy.rs`（新規）——`archive_password_mode`（`config set`で設定する
    既存のapp_setting汎用機構をそのまま利用、専用サブコマンドは追加していない）が`try_registered`
    の場合のみ、`--password-store`必須（未指定は実行失敗=コード3）・標準入力がTTYでなければ
    コード4（§10.16の「対話入力が必要だがTTYでない」）・TTYならマスターパスワードを1行読み取り
    `UnlockedStore::unlock`。`--no-archive-password`はこの解決全体をスキップし常にモード1
    （§10.16の該当オプション）。登録パスワードの追加・削除やマスターパスワードの設定・変更・
    リセット自体はGUI専用のまま（§10.16の「CLIで提供しないもの」を変更していない——CLIは
    あくまで既存の設定ファイルを「使う」側で、「作る・管理する」側の操作は持たない）。
- テスト結果:
  - `cargo test --workspace`: 99件全てpassed（core 64+1+13+3件＋CLI 18件）。
    - `secrets::tests`（8件、新規）: create→unlock往復、誤マスターパスワード拒否、
      作成済みストアへの`create`拒否、パスワード削除、マスターパスワード変更後は新パスワードのみ
      有効、reset（不在時もエラーにならないこと含む）、暗号文改ざんが`Corrupt`として検出される
      こと。
    - `archive::tests`に6件追加: 暗号化zip/7zそれぞれについて、Rejectポリシーでのエラー・正しい
      登録パスワードでの復号成功（誤パスワードが先に来ても正しいものに到達すること）・
      登録パスワードが1つも一致しない場合のエラー。
    - CLI統合テスト4件（新規）: 暗号化アーカイブがデフォルト（モード1相当）ではハッシュエラー
      1件として報告されること（`reference generate`の終了コード2・`errors: 1`)、
      `--no-archive-password`が`archive_password_mode=try_registered`設定時でも優先されること
      （プロンプトなし・`--password-store`未指定でもエラーにならない）、`try_registered`設定済み
      かつ`--password-store`未指定は実行失敗（コード3）、`try_registered`設定済みかつ非TTYは
      コード4。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`
    いずれも成功・警告なし。
- 問題・注意点:
  - マスターパスワードのTTY入力は行読み取りのみ（非表示入力にはならない）。`rpassword`等の追加
    導入は行わず、既存依存を増やさない範囲に留めた（将来必要になれば追加検討）。
  - CLIからの実際のTTY経由マスターパスワード入力の成功パス（正しいパスワードで実際に復号できる
    こと）は自動テスト不可（サブプロセスに疑似TTYを与える手段がないため）。P8の`--mount`
    フォールバック同様、非TTY時の失敗パス（コード4）のみ自動テストで確認し、成功パス自体は
    コアの`secrets`/`archive`単体テストが暗号ロジックそのものを別途検証している。
  - 登録パスワードの管理（追加・削除）・マスターパスワードの設定/変更/リセットのGUI画面自体は
    P12（GUI）で実装する。
- 状態: 完了。次はP11（再構成機能、reconstruct）。

## 2026-09-04 P11: 再構成機能（reconstruct）

- 実施内容:
  - `crates/core/src/db/schema.rs`: §10.20で設計済みの`reconstruction_run`/
    `reconstruction_item`（2テーブル）を追加（P2時点では「P11で追加」と明記して保留していたもの）。
    `ReconstructionItemStatus`（pending/written/error）を`models.rs`に追加、`reconstruction_run.status`
    は既存の`RunStatus`（scan_run/check_runと共通）をそのまま再利用。
  - `crates/core/src/archive/deterministic.rs`（新規）: TorrentZip（zip）・RV7Z（7z）の決定的生成。
    - **TorrentZip**: `docs/TorrentZip_Torrent7z仕様調査.md` §1の固定値（version needed=20、
      general purpose flag=2、method=8、日時固定値48128/8600、extra長=0等）をそのまま実装した
      自前のzipシリアライザ（`zip`クレートのwriterでは個々のヘッダ値を直接制御できないため）。
      ソート順は`TrrntZipStringCompare`（ASCII A-Z のみ小文字化して比較→タイなら元の大小文字で
      比較、の2段階）。EOCDコメント`TORRENTZIPPED-XXXXXXXX`はcentral directory自体のCRC32。
    - **RV7Z**: 同資料§3.2のRomVault現行方式（Solid-LZMA、method ID `03,01,01`、
      `Trrnt7ZipStringCompare`＝拡張子→ファイル名→ディレクトリパスの順、タイムスタンプなし）。
      7zコンテナ自体の生成は`sevenz-rust2`の`prepare_block`/`push_prepared_block`
      （複数エントリを1つのsolidブロックにまとめて圧縮するAPI）を利用し、末尾に
      `RomVault7Z0`+バリアント1桁+ヘッダCRC(4B)+ヘッダ位置(8B)+ヘッダ長(8B)の検証用トレーラを
      追記（この3フィールドは標準7zのsignature headerが元々持つ`NextHeaderCRC`/`Offset`/`Size`
      をそのまま転記したもので、どのエンコーダが作った7zでも読み取れる）。
    - **正直な限界の明記**（モジュールdocコメントに記載）: 自前実装のTorrentZipシリアライザ、
      および`sevenz-rust2`のLZMAエンコーダは、RomVault実装とは別のエンコーダであるため、
      「圧縮パラメータ・コンテナ構造は仕様通り」であることと「実際のRomVaultバイナリ出力との
      バイト完全一致」は別問題であり、この環境には比較対象となる実際のRomVault出力がなく
      後者は未検証。検証済みなのは「自分自身の出力が実行するたびに毎回バイト一致すること」
      （決定性）と「自分自身の`archive`モジュールで正しく読み戻せること」（往復整合性）の2点。
    - `archive::read_entry_content`（新規、`hash_entry`と同じhopウォークだがハッシュせず生バイト
      列を返す）を追加——再構成が実際の書き出しバイト列を必要とするための、hash_entryには
      なかった機能。
  - `crates/core/src/reconstruct/mod.rs`（新規）:
    - `compute_plan`: §10.20で確定した優先順位ルール（再構成先→他の非リムーバブル→リムーバブル
      は最新スキャン優先）をSHA-256一致で適用し、新規のintegrity種別`check_run`
      （`integrity_check_result`にok/missingを記録）として結果を残す。マッチングは
      **パスではなくハッシュ**（再構成の本質は「内容が正しいコピーをどこからでも見つける」ことで
      あり、ソース側の元のファイル名・配置は問わない）。フォルダ由来の候補で未ハッシュのもの
      （§10.2/§10.3の遅延パス）はこの場で計算・永続化する（duplicate/integrity比較フェーズと
      同じ扱い）。リムーバブルメディア由来は§10.8のeagerモードにより常にハッシュ済みなので、
      計画段階でメディア接続は不要。
    - **アーカイブ入れ子の扱い**: あるお手本セットエントリ（例: `game.zip`）が他のエントリ
      （`game.zip/a.bin`）のコンテナである場合、コンテナ自身は「単独ファイルとしてどこかから
      調達する」対象から除外する（自分自身が生成する決定的アーカイブと寸分違わず一致する外部
      ファイルは存在し得ないため）。二重入れ子（`outer.zip/inner.zip/leaf.txt`）は`inner.zip`を
      再構築せず`outer.zip`直下の1エントリとして扱う簡略化とし、モジュールdocに明記。
    - `create_run`/`run_pass`: 充当計画のうち解決済み分のみ`reconstruction_item`化
      （missingはそもそも行を作らない＝§10.20の「部分的に再構成」通り）。実行パスは
      ルースファイル（コンテナに属さない参照ファイル）とコンテナ単位（同一コンテナ配下の
      エントリをまとめて1つの新規アーカイブとして書き出す）を分けて処理し、
      コンテナは全メンバーがそのパスで揃って初めて書き出す（リムーバブルメディア未接続で
      1つでも欠けていれば、そのコンテナ全体を次パスまで保留）。書き込みは§10.24/7.4の決定通り
      無条件上書き。エラー・未接続分は次回`run_pass`呼び出しで自動的に再試行される
      （`pending`と`error`の両方を毎回処理対象にすることで、§10.20の「そのメディアで必要な
      全ファイルの試行が終わった時点で失敗分のみ再試行」をCLI側の複数回呼び出しだけで実現）。
  - CLI: `reconstruct plan --check-run <ID> --destination <PATH>`（計画のみ、DB書き込みは
    新規check_run/integrity_check_resultのみでreconstruction_runは作らない）・
    `reconstruct run (<RECONSTRUCTION_RUN_ID> | --check-run <ID> --destination <PATH>)`
    （新規実行または既存runの再開、メディア入れ替えの対話ループ——非TTYならその場で打ち切り報告）・
    `reconstruct status <ID>`。P10と同じパスワードポリシー（`--password-store`/
    `--no-archive-password`）をスキャン・ハッシュ計算全体に適用。
- テスト結果:
  - `cargo test --workspace`: 117件全てpassed（core 72+1+13+6+3件＋CLI 22件）。
    - `archive::deterministic::tests`（8件、新規）: TorrentZip/RV7Zそれぞれの決定性
      （同一入力→2回生成しバイト一致）、往復読み取り（`zip`/`sevenz_rust2`自身のreaderで
      読み返せること）、ソート順の実地検証、空エントリリストの処理、RV7Zトレーラの
      NextHeaderCRC一致検証。
    - `reconstruct::tests`はモジュール内テストなし（統合テストで代替）、
      `crates/core/tests/reconstruct.rs`（6件、新規）: 単一ソースからの解決・書き出し、
      解決不能エントリがあっても他は書き出される（部分再構成）、再構成先ローカルコピーが
      ライブラリより優先されること、同名別内容ファイルの無条件上書き、アーカイブ入れ子
      エントリが新規コンテナへ再構成されること（実際に`zip`クレートで読み戻して内容・
      TorrentZipコメントを検証）、リムーバブルメディア未接続時は該当分のみ保留され接続後の
      再パスで解決されること。
    - CLI統合テスト4件（新規）: plan→run→statusの一連の流れ（本物のファイルシステムに書き出され
      内容が一致すること、再開run呼び出しが安全なno-opであること）、run引数の必須組み合わせ
      検証（コード64）、未解決エントリがあってもコマンド自体は失敗しないこと（コード1、
      `未解決: 2`表示）、duplicate種別のcheck_runを渡した場合の拒否（コード3）。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`
    いずれも成功・警告なし。
  - 手動スモークテスト: 実際のCLIバイナリで、(1) 単純な2ファイルの再構成、(2) 元のzipから
    バラバラに散らばったファイル群を新規TorrentZipへ再構成し、Pythonの`zipfile`モジュールで
    独立に内容・コメントを検証、の2パターンを実行して確認。
- 問題・注意点:
  - `run_integrity_check`自体（P5由来、複数scan_runがパス一致した場合の一般優先順位ルール）は
    今回改修していない。§10.20の優先順位ルールは、実際には「複数scan_runがパス一致した場合の
    一般ルール」ではなく「複数scan_runがハッシュ一致した場合の再構成専用ロジック」として実装
    した（`reconstruct::compute_plan`が独自に新規check_runを作りハッシュベースでマッチングする
    ため、既存の整合性チェックのパスベースマッチングとは別経路）。一般の`check_run`が複数
    scan_runでパス一致した場合に同ルールを適用する改修は、既存のP5テストへの影響を避けるため
    今回は見送った（将来必要になれば別途対応）。
  - `list_ok_scanned_files_for_scan_runs`/`list_scanned_files_for_integrity`
    （P4/P5由来）はリムーバブルメディアのscan_runを対象から除外したままであることを確認した
    （`check duplicate --media-id`は§10.16で決定済みのオプションだが未実装のまま）。再構成専用の
    `list_scanned_files_for_reconstruction`は今回この制限を持たない新規クエリとして実装した
    ため、再構成機能自体はリムーバブルメディア由来のファイルを正しく候補に含められる。
  - マスターパスワード同様、リムーバブルメディア入れ替えの対話ループの「実際に接続してEnterを
    押すと次のメディアを処理する」という成功パス自体は自動テスト不可（疑似TTYが必要なため）。
    非TTY時の即時打ち切り・複数パスでの自動再試行ロジック自体は自動テストで確認済み。
- 状態: 完了。次はP12（GUI、Tauri）。

## 2026-09-05 P12: GUI（Tauri）

- 実施内容:
  - `crates/gui`（新規、`filechecker-gui`）: Tauri v2アプリ。ワークスペースに追加
    （`Cargo.toml`の`members`）。
    - `Cargo.toml`/`build.rs`/`tauri.conf.json`/`capabilities/default.json`/`icons/`
      （Pillowで生成した単色プレースホルダアイコン一式、32/128/128@2x/ico/icns/Windows
      Storeロゴ各サイズ）。`tauri.conf.json`は`app.withGlobalTauri: true`——npm・
      フロントエンドのビルドステップを一切持たず、`window.__TAURI__.core.invoke`/
      `window.__TAURI__.dialog`をグローバルに公開させることで、`dist/`配下の素のHTML/
      CSS/JSだけでフロントエンドを完結させる方針（後述）。
    - `src/state.rs`: `AppState`——結果DB接続（`Mutex<Connection>`）と、登録パスワード
      設定ファイル（§10.9/§10.10）の解錠状態（`Mutex<Option<UnlockedStore>>`）・パス。
      DB・パスワード設定ファイルとも`app.path().app_data_dir()`配下に固定
      （§10.9の「アプリ内で完結する簡易な管理」）——CLIの`--db`必須方針とは異なり、GUIは
      OS標準のアプリデータフォルダに既定パスを持つ。
    - `src/commands/`（画面群ごとにモジュール分割、全て`filechecker_core`への薄いラッパ
      —§10.13「CLIはGUIの機能の部分集合」の逆、GUI層にもチェック/スキャンロジックを
      持たせない）: `home.rs`（ホーム画面のサマリ集計）、`reference.rs`（お手本セット
      一覧・フォルダスキャン生成・MAME取り込み）、`check.rs`（整合性・重複チェック実行/
      結果一覧/CSV・JSON出力、`check_list`）、`history.rs`（スキャン履歴・単体フォルダ
      スキャン）、`media.rs`（既知メディア一覧・接続中メディア検出・メディアスキャン、
      §10.21の自動識別失敗時はGUIのネイティブダイアログで直接ラベルを受け取るためCLIの
      TTYフォールバックは不要）、`settings.rs`（全般設定get/set、登録パスワード管理・
      マスターパスワード初回設定/入力/変更/リセット）、`reconstruct.rs`（充当計画・
      実行・状況、§10.20）、`helpers.rs`（共通ヘルパー）。
    - `src/commands/helpers.rs`の`with_password_policy`: CLIの`password_policy::resolve`
      （プロセス起動ごとに1回だけ解決）とは異なる設計。GUIは長寿命プロセスで専用の
      パスワード管理画面を持つため、`archive_password_mode=try_registered`かつストアが
      解錠済みならその場で`PasswordPolicy::TryRegistered`を都度組み立てて使い、未解錠なら
      「先にマスターパスワードを入力してください」エラーを返す（フロントエンド側で
      マスターパスワード入力モーダルを出してリトライする、後述の`invokeWithPasswordRetry`）。
      解錠した鍵は`password_store_lock`を呼ぶかアプリ終了までメモリ上に保持され続ける
      （§10.10の「鍵はメモリ上のみ保持」を、GUIでは「操作の都度」ではなく「セッション中」
      の粒度で満たす設計判断——都度マスターパスワード入力を要求すると連続スキャン時の
      UXが大きく損なわれるため）。
    - core側の変更（GUIのJSON IPC向け、いずれも後方互換な追加のみ）:
      `db::models`の全enum（`TargetType`/`HashMode`/`RunStatus`/`FileStatus`/`CheckType`/
      `ReconstructionItemStatus`/`ResultStatus`）に`#[derive(Serialize)]`
      （`rename_all="snake_case"`、既存の`as_str()`出力と一致）、`db::repo`の主要な行構造体
      （`RemovableMediaRow`/`ScanRunRow`/`ReferenceSetRow`/`ReferenceFileRow`/
      `CheckRunRow`/`IntegrityResultRow`/`DuplicateGroupRow`/`DuplicateGroupMemberRow`/
      `ReconstructionRunRow`/`ReconstructionItemRow`/`ReconstructionItemCounts`）と
      各モジュールのサマリ構造体（`ScanSummary`/`DuplicateCheckSummary`/
      `IntegrityCheckSummary`/`GenerateReferenceSetSummary`/`ImportSummary`/
      `media::DetectedMedia`/`reconstruct::{Plan, ResolvedItem, PassSummary}`）にも
      `Serialize`を追加。加えて`db::repo::list_scan_runs`
      （新規、`ScanRunSummaryRow`——スキャン履歴画面向けにファイル数・リムーバブル
      メディア表示名をJOINしたサマリ行）と`HashMode::parse_str`（既存の`as_str`の逆、
      このクエリの行マッピングに必要）を追加。SHA-256等のハッシュ値バイト列
      （`Vec<u8>`）はJSONでは16進文字列の方が扱いやすいため、`ReferenceFileRow`/
      `DuplicateGroupRow`をそのまま返す代わりに`commands`層で`hex_encode`した
      DTO（`DuplicateGroupDto`等）に詰め替えて返す設計にした（core側の型はDB内部表現の
      ままとし、GUI都合のシリアライズ形式をGUI層に閉じ込める）。
    - フロントエンド（`crates/gui/dist/`、vanilla HTML/CSS/JS、ビルドステップなし）:
      `index.html`（画面ごとの`<template>`を全てインライン定義）・`style.css`・
      `app.js`（画面遷移・DOM描画・`invoke()`呼び出しのみを担う単一ファイル）。
      §10.14のワイヤーフレーム通り、ホーム／お手本セット一覧・作成（タブ: フォルダ
      スキャン/外部インポート）／整合性チェック実行設定・結果一覧（ステータス別
      バッジ・フィルタ・検索・📦アーカイブ表示）／重複チェック対象設定（フォルダ・
      リムーバブルメディア・スキャン履歴の混在）・結果一覧（グループ展開）／
      リムーバブルメディア管理／スキャン履歴／設定（全般・登録パスワード管理・
      マスターパスワード各モーダル）／再構成（充当計画・実行中）の各画面を実装。
      `tauri-plugin-dialog`のネイティブフォルダ/ファイル選択・保存ダイアログを使用。
    - **アーカイブ内エントリの📦表示**（§10.14）: `IntegrityResultRow`が
      `parent_archive_file_id`/`archive_depth`を持たないため、フロントエンド側で
      パスの各セグメントが`.zip`/`.7z`拡張子で終わるか（バックエンドの
      `ArchiveFormat::detect`と同じ拡張子ベース判定）を見て、通常フォルダのネストと
      アーカイブのネストを区別する簡易ヒューリスティックを採用（`app.js`の
      `renderPath`にコメントで理由を明記）。
  - テスト結果:
    - `cargo test --workspace`: 129件全てpassed（P0-P11の117件＋GUI新規12件）。
      GUI側は`filechecker_core`への委譲がほとんどで（既存テストがロジック自体を
      カバー済み）、GUI固有の非自明なロジックのみを対象にunitテストを追加:
      `check.rs`（4件、CSV/JSON出力のカンマエスケープ・サイズ欠損時の空欄・
      JSON往復・`resolve_scan_run_ids`のバリデーション分岐）、`helpers.rs`（3件、
      `hex_encode`・`with_password_policy`の既定Reject/ロック中エラー）、
      `reconstruct.rs`（4件、`resolve_destination`の排他検証・存在しないフォルダ・
      既存scan_run再利用・リムーバブルメディアscan_run拒否）、`gui-core`側の
      新規`list_scan_runs`は手動E2E（後述）で実データを通して確認。
    - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D
      warnings`いずれも成功・警告なし。
    - **実際に起動しての目視確認**（計画の「テスト戦略まとめ」表のGUI行）:
      このサンドボックス環境にはGUI表示系が一切入っていなかったため、
      `libwebkit2gtk-4.1-dev`/`libgtk-3-dev`/`libayatana-appindicator3-dev`/
      `librsvg2-dev`/`libsoup-3.0-dev`（Tauri Linuxビルド要件）・`cargo install
      tauri-cli`・（目視確認専用に）`Xvfb`/`scrot`/`xdotool`/`fonts-noto-cjk`を
      その場でaptインストールした上で、Xvfb仮想ディスプレイ上で実バイナリを起動し、
      `xdotool`でクリック操作、`scrot`でスクリーンショットを撮って内容を確認する
      という手順で実施した（詳細は下記CI追記を参照——ubuntu-latest CIランナーにも
      同じLinux依存が必要なため`ci.yml`に追加した）。
      - ホーム→整合性チェック（お手本セット一覧）→お手本セット作成画面まで遷移し
        UIが正しく描画されること、ネイティブのGTKフォルダ選択ダイアログが実際に
        開くことを確認。
      - 実際のCLIバイナリでGUIと同じSQLiteファイル（GUI起動時に作成される
        `app_data_dir`配下の`filechecker.sqlite3`）に対し`scan folder`→
        `reference generate`を実行し、GUIを再読み込みしてホーム画面の
        「1件のお手本セット」・お手本セット一覧・整合性チェック実行設定画面の
        スキャン履歴プルダウンにその内容が正しく反映されることを確認
        （GUI/CLIが同一DBファイルを問題なく共有できることの実地確認）。
      - その状態から実際に整合性チェックを実行し（スキャン履歴の既存scan_run
        再利用）、結果画面でOK:2/破損:0/欠落:0/余剰:0/エラー:0のバッジと
        「整合性チェックが完了しました」トーストを確認。同様に重複チェックも
        実行し、グループ数0・エラー0で完了することを確認。
      - 設定画面の「登録パスワード管理」タブで実際にマスターパスワード初回設定
        モーダルを操作し、Argon2id/AES-GCMの登録パスワード設定ファイルが
        `app_data_dir`に実際に生成されること（内容をダンプして
        `kdf_salt`/`verifier`/`nonce`/`ciphertext`の構造を確認）、続けて
        登録パスワードの追加・一覧表示（マスク表示）が動作することを確認。
      - **この過程で実装バグを2件発見・修正**（テストが実際に役立った例）:
        (1) `index.html`に`<meta charset="utf-8">`が無く、WebKitGTKが日本語
        テキストを文字化けさせていた（Latin-1相当として解釈されていた）。
        (2) `style.css`に`[hidden]`を明示的に扱うルールが無かったため、
        `class="row"`など`display`を指定するクラスを持つ要素に`hidden`属性を
        付けても隠れない（UA既定の`[hidden]{display:none}`と作者側の
        `.row{display:flex}`が同じ詳細度で衝突し、作者側ルールが勝つ）バグが
        あり、設定画面のパスワード管理パネルで「未作成/ロック中」でも操作
        ボタン一式が誤って表示され続けていた。`[hidden]{display:none!important}`
        を追加して解消——他の`hidden`切り替え箇所（モーダルの`current-row`等）
        にも同様の潜在バグがあったはずで、今回のグローバル修正で合わせて解消。
      - **開発ループ上の注意点**（作業中に判明、次フェーズ以降のフロントエンド
        修正時のために記録）: `cargo build`/`cargo run`だけではフロントエンド
        （`dist/`）のみの変更が確実に再埋め込みされないことがあった
        （`tauri::generate_context!()`のプロク・マクロ展開はlib.rs自体の再
        コンパイルに紐付くため、Rustソース側に変更が無いとcargoがビルドスクリプト
        の再実行やマクロ再展開自体をスキップし、古い`dist/`内容がバイナリに
        残ったままになるケースがあった）。フロントエンドのみを変更した場合は
        `crates/gui/src/lib.rs`を`touch`するか、素の`cargo build`ではなく
        `cargo tauri dev`/`cargo tauri build`を使うこと。
    - **`.github/workflows/ci.yml`を更新**: `ubuntu-latest`のみに条件付きの
      Tauri Linuxビルド依存インストールステップ（`libwebkit2gtk-4.1-dev`/
      `libgtk-3-dev`/`libayatana-appindicator3-dev`/`librsvg2-dev`/
      `libsoup-3.0-dev`）を追加。windows-latest/macos-latestは追加インストール
      不要（WebView2/システムWebKitがランナーに標準搭載）という前提で変更していない
      ——実際の3OS CI結果はPR作成後に確認する（P0以来の既存の運用方針を踏襲）。
  - 問題・注意点（簡略化した点、将来フェーズで補完予定）:
    - **進捗イベントなし**: 「実行中」画面（§10.14、情報取得フェーズ→比較フェーズの
      粒度別プログレス表示）は、`invoke()`のPromiseを待つ間スピナー表示するのみに
      留めた。`scan_folder`/`run_integrity_check`等のcore関数がフェーズ単位の
      進捗コールバックを持たないため、真の段階的プログレスはcore側にフックを
      追加する改修が必要（CLIも同様に「開始・完了の1行のみ」でP6時点から
      簡略化されており、GUIも同じ制約を引き継いだ）。
    - **アーカイブ展開失敗の折りたたみ集約表示（§10.15）は未実装**: P6/P7の
      progress-logで記録済みの通り表示層はP13想定。今回のGUIも個々の行を
      フラット表示するのみで、`detail`＋`parent_archive_file_id`が同じ行を
      1行に集約する機能は追加していない（DB側は集約可能な形で記録済み・
      表示層追加はブロックされない、という既存の判断を維持）。
    - **完了報告画面をGUI「実行中」画面に統合**: §10.14は充当計画／実行中
      （メディア入れ替え）／完了報告の3画面構成だが、実装では完了報告用の
      個別画面を作らず、`reconstruct-progress`画面がそのまま完了後もカウント
      とトーストを表示する形に統合した。書き出し件数・使用メディア内訳の
      CSV/JSON/HTML出力は、再構成が内部的に生成する整合性`check_run`
      （`reconstruct::compute_plan`が作るもの）に対して既存の`report_export`
      をそのまま使う想定（HTML自体は他画面同様P13待ち）。
    - **HTML出力は引き続き未実装**（CSV/JSONのみ）。CLIのP6時点の制限をGUIでも
      そのまま踏襲——P13で横断的に追加する。
    - Windows/macOSのリムーバブルメディア識別バックエンドはP8のプレースホルダの
      まま（空リストを返す）。GUIのメディア管理・対象設定画面もこれに従い、
      両OSでは接続中メディアが常に0件として表示される（誤検知よりは安全側、
      という既存の設計判断をGUI層でも維持）。
    - Tauri公式のE2Eテストツール（WebDriver等）は導入していない。理由は
      上記の通り実際にXvfb上で起動して目視確認する形で代替したため——CI環境で
      毎回Xvfbを使った自動E2Eを走らせる仕組み自体は今回作っていない
      （必要になれば別途導入を検討）。
    - GUIからのマスターパスワード変更モーダルは「現在のマスターパスワード」を
      毎回`UnlockedStore::unlock`で独立検証してから切り替える設計とした
      （既にメモリ上に解錠済みのストアがあっても、変更操作自体は再認証を要求する
      ——CLIには存在しない操作のため独自に設計判断）。
- 状態: 完了。次はP13（レポート出力・横断仕上げ）。

## 2026-09-05 P13: レポート出力・横断仕上げ

- 実施内容:
  - **HTML出力**（§10.14/10.16）: CLI `report export --format html`とGUI
    `report_export`（`format: "html"`）の両方に追加。既存のcsv/json実装と対になる
    `render_integrity_html`/`render_duplicate_html`（CLI: `crates/cli/src/
    output.rs`、GUI: `crates/gui/src/commands/check.rs`）を素朴な`<table>`＋インライン
    styleのスタンドアロンHTMLとして実装（外部CSS/JS依存なし）。既存のcsv/json同様、
    CLI・GUIそれぞれで別実装のまま——§10.16の「表示内容は共通、表現形式はCLI/GUIそれぞれ」
    という既存の切り分け方針（P6/P12から継続）を踏襲し、今回新たに共通化リファクタは
    行っていない。
  - **htmlの提供範囲の制限**（§10.16「HTMLは`report export`側でのみ提供」）: CLI側は
    `Format`列挙体に`Html`を追加した上で、`check integrity`/`check duplicate`/
    `check show`の3コマンドは`reject_html_for_stdout`で明示的に拒否（終了コード64）。
    `check show`は内部的に`report export`からも呼ばれる共有関数のため、CLIサブコマンド
    としての`check show`だけ拒否するガード付きラッパー`check_show_cli`を新設し、
    `report_export`はガードなしの内部関数を直接呼ぶ形に分離した。
  - **エラーログファイル**（§10.17/§10.22）: 新設`crates/core/src/errorlog`。個々の
    scan/hash呼び出し経路に生きたロガーを引き回す設計（P10の`PasswordPolicy`と同様の
    `_with_password_policy`方式をさらに`_with_error_log`的な変種で増殖させる案）は、
    既存シグネチャへの影響が大きい割に得られる情報が薄いため採用しなかった——本
    リポジトリの「詳細診断情報」は結局`scanned_file.error_message`/
    `integrity_check_result.detail`という一行サマリの中（`{err}`のDisplayで元のOS
    エラーまで含む）にしか存在せず、リトライ回数も固定定数でインスタンスごとの
    情報を持たないため、呼び出し時点でログを書いても事後にDBから再構成しても得られる
    情報は同一。そこで**DBに書き込み済みのエラー行から事後に読み直して書き出す**設計
    にした（`write_scan_run_log`: 対象scan_runの`status='error'`な`scanned_file`行から、
    `write_check_run_log`: 整合性チェックは`integrity_check_result`の`error`行から、
    重複チェックは`check_run_source`経由で辿った各scan_runの`status='error'`な
    `scanned_file`行から——重複チェックはcheck_run単位でエラー件数を永続化していない
    というP6からの既知の制約があるが、比較フェーズのハッシュエラーは
    `mark_scanned_file_error`で結局`scanned_file`に書き戻されるため、P11の再構成機能
    と同じsource-scan-run経由のJOINで拾える）。エラーが1件もないrunはログファイル
    自体を作らない（クリーンなrunに読み手のいない空ファイルを量産しないため）。
    CLIには新規グローバル引数`--log-dir <DIR>`を追加し、指定時のみ`scan folder`/
    `scan media`/`reference generate`/`check integrity`/`check duplicate`の後段で
    書き出す。GUI側は今回未着手（CLIの`--password-store`同様、CLI/GUIどちらで
    エラーログを有効化するかは呼び出し側の明示指定に委ねる設計だが、GUIの
    `app_data_dir`配下への自動配線はP13のスコープ外とした——次にGUI側のエラー表示
    /診断機能に着手する際、`errorlog::write_scan_run_log`/`write_check_run_log`を
    そのままGUI commandsから呼べる形で用意済み）。
  - 各writeは対象run（scan_run/check_run）の**現在の完全なエラー集合**をDBから毎回
    再取得して書き出すため、同一runに対する2回目以降の呼び出しは追記ではなく
    **上書き**にした（例: `scan folder`実行後に`reference generate`が同じscan_runに
    対して追加のエラーを発生させた場合、両方のログ書き込みが同じ完全集合を返すため、
    素朴な追記実装だと1回目に書いた行が2回目でも重複して書かれてしまうバグに実装中に
    気づき、修正した——実装ノートを`errorlog`モジュール冒頭に明記）。
  - `docs/implementation-plan.md`のP13チェックボックスをすべて✅に更新。
- テスト結果:
  - core: `errorlog`モジュールに3テスト追加（エラーなしrunはファイル未作成、1行書き出しの
    内容確認、2回書き出しても重複しないことの確認）。core全体75件、既存分含めすべてpass。
  - CLI（`crates/cli/tests/cli.rs`、実バイナリをサブプロセス起動する既存方式）:
    `report_export_supports_html_and_check_show_rejects_it`（htmlのreport export成功と
    check showでの拒否）、`log_dir_writes_a_scan_run_error_log_only_when_there_are_errors`
    （`#[cfg(unix)]`、既存の`unreadable_matched_file_yields_exit_code_2`と同じ
    root実行時スキップパターン）、`report_export_handles_several_thousand_rows_without_
    pathological_slowdown`（実装計画の「大量件数でのエクスポート性能」向けの軽量な
    健全性テスト——4000ファイルを実際に書き込み・スキャン・変更・再スキャンして
    corrupted 4000件を作り、csv/json/htmlそれぞれのreport exportが10秒未満で完了し
    件数が一致することを確認。TBクラス/数十万〜百万ファイル規模での本格的な負荷試験
    はP14の役割であり、本テストは「実装がうっかりO(n²)になっていないか」程度の
    軽い健全性チェックに留めている旨をテストのコメントに明記）を追加。CLI全体25件、
    既存分含めすべてpass。
  - GUI（`crates/gui/src/commands/check.rs`のユニットテスト）:
    `integrity_export_html_escapes_and_includes_every_row`を追加（HTMLエスケープと
    行内容の確認）。GUI全体13件、既存分含めすべてpass。
  - `cargo fmt --all -- --check`・`cargo clippy --workspace --all-targets -- -D warnings`
    ともにクリーン。
- 問題・注意点:
  - GUIの`--log-dir`相当（エラーログファイルの出力先）は今回未配線（上記参照）。
  - GUIのHTML出力ボタン（`dist/index.html`の「HTML出力」、`dist/app.js`の
    `export-html`/`export-dup-html`）はコード上は追加したが、Tauriアプリを実際に
    Xvfb上で起動しての目視確認はP12のような形では今回実施していない——csv/json
    ボタンと同じ`exportCheck()`関数を再利用する構造上のリスクは低いと判断したが、
    フロントエンドのみの変更が`cargo build`でバイナリに反映されない場合がある
    というP12の既知の注意点（`crates/gui/src/lib.rs`のtouchか`cargo tauri build`が
    必要）が次回のGUI起動確認時に該当する点は申し送りとする。
  - エラーログの「レベル」欄は常に`ERROR`固定（このコードベースにはこれ以外の
    レベルが実際には発生しないため）。「タイムスタンプ」欄は当該run全体の
    実行時刻を全行で共通使用する（DBはファイル単位のエラー発生時刻を別途保持
    していないため、ファイル単位で異なる値を捏造せず、正直に取得可能な粒度に
    留めた）。
- 状態: 完了。次はP14（非機能要件の検証・性能/メモリ）。
