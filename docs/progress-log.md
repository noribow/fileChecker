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
