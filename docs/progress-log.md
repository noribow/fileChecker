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
